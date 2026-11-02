// SPDX-License-Identifier: AGPL-3.0-only
//! One game process, from identification to restoration, as a typestate.
//!
//! ```text
//! Candidate ─identify→ Identified ─claim→ Claimed ─prepare→ Prepared ─install→ Installed ─restore→ Detached
//!                                                                         └─────────────┴──→ Recovery ─restore→ Detached
//! ```
//!
//! - [`identify`] proves the process is the supported build without changing anything. A [`ReadOnly`]
//!   candidate stops there; that is the `check` command.
//! - [`Session::claim`] takes the per-process owner and stop objects; a second trainer is refused.
//! - [`Session::prepare`] maps the zeroed control word into the game, places the cave block within jump range
//!   of every site, and writes, checks and protects every cave. Nothing can reach the caves yet, so a failure
//!   here frees them.
//! - [`Session::install`] freezes the game and writes the 17 site jumps with every cheat off. Before the first
//!   write the cave block is given up for good: from then on a game thread may run it, so it stays until the
//!   game exits.
//! - [`Session::publish`] stores a mask in the control word. It never freezes the game or touches code.
//! - [`Session::restore`] publishes zero, then freezes the game and puts every site back.
//! - A failed install or restore leaves a [`Recovery`] session holding whatever the game may still have; it
//!   publishes nothing and can only try to restore again.
//! - [`Session::exited`] ends a session whose game has exited, without touching the game.
//!
//! The process behind a session is a private seam: the Windows backend in production, a pretend process in
//! tests.
#![forbid(unsafe_code)]

#[cfg(test)]
pub(crate) mod fake;
mod identify;
mod prepare;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows;

pub use identify::{IdentifyError, SiteCheck, SiteStatus};
pub use prepare::PrepareError;
#[cfg(windows)]
pub use windows::{SelectError, find, request_stop, select};

use std::fmt;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::addr::{CodeRange, Va};
use crate::cave::HookPlan;
use crate::control::{Ack, ControlMask, DesiredMask};
use crate::identity::Sha256Digest;
use crate::patch::freeze::{FreezeError, ResumeError};
use crate::patch::{
    self, ApplyError, Freeze, OsError, PatchJournal, Process, RestoreIncomplete, Stuck,
};
use crate::profile::{ModSlug, SiteId, ValidProfile};
use crate::recovery::RecoveryAction;

/// How often a freeze is tried before giving up.
const FREEZE_ATTEMPTS: u32 = 100;
/// How long to wait between freeze attempts and between rollback attempts.
const RETRY_WAIT: Duration = Duration::from_millis(10);
/// How often a failed install or restore is retried while the game stays frozen.
const ROLLBACK_ATTEMPTS: u32 = 10;

/// The calls a session makes on a game process, beyond the patch engine's.
pub(crate) trait Game: Process {
    /// The executable's full path, as the process reports it.
    fn image_path(&self) -> Result<PathBuf, OsError>;
    /// The executable file's contents, read with every share mode so the game can keep it open. Files over
    /// `limit` bytes are refused.
    fn read_executable(&self, path: &Path, limit: u64) -> Result<Vec<u8>, OsError>;
    /// The main module: the first in the module list.
    fn main_module(&self) -> Result<Module, OsError>;
    /// Whether the process has exited.
    fn exited(&self) -> bool;
    /// Waits up to `timeout`, returning early if the process exits.
    fn wait(&self, timeout: Duration);
    /// The allocation granularity.
    fn granularity(&self) -> u64;
    /// The highest address a process can use.
    fn highest_address(&self) -> u64;
    /// Whether this trainer was asked to shut down, by a closing window, logoff or Ctrl+C.
    fn interrupted(&self) -> bool;
    /// Reserves and commits `len` read-write bytes at exactly `at`.
    fn allocate(&self, at: Va, len: u64) -> Result<Box<dyn Allocation>, OsError>;
    /// Maps a zeroed control word into the game.
    fn controls(&self) -> Result<Box<dyn Controls>, OsError>;
    /// Takes this process's owner and stop objects for `slug`.
    fn claim(&self, slug: ModSlug) -> Result<Box<dyn Owner>, ClaimError>;
}

/// The main module of a process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Module {
    pub(crate) path: PathBuf,
    pub(crate) base: Va,
    pub(crate) size: u32,
}

/// Memory allocated in the game. Dropping it frees it.
pub(crate) trait Allocation {
    /// Its first byte.
    fn base(&self) -> Va;
    /// Gives it up for good: it stays allocated until the game exits.
    fn retain(self: Box<Self>);
}

/// The control word shared with the game.
pub(crate) trait Controls {
    /// Its address in the game.
    fn remote(&self) -> Va;
    /// Stores `word` with one interlocked 32-bit store.
    fn store(&self, word: u32);
    /// Reports that the session's restoration finished, whatever its outcome, so a close request waiting for
    /// it can let the trainer exit. Dropping the controls zeroes them and unmaps the trainer's view; the game's
    /// view stays.
    fn finished(&self);
}

/// The objects proving this trainer owns the process.
pub(crate) trait Owner {
    /// Whether someone asked this trainer to stop.
    fn stop_requested(&self) -> bool;
}

/// Read access only: a `check`.
#[derive(Debug)]
pub struct ReadOnly;
/// Read and write access: an attach.
#[derive(Debug)]
pub struct Write;

/// The access a process was opened with.
pub trait Access: sealed::Sealed {}
impl Access for ReadOnly {}
impl Access for Write {}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::ReadOnly {}
    impl Sealed for super::Write {}
    impl Sealed for super::Identified {}
    impl Sealed for super::Claimed {}
    impl Sealed for super::Prepared {}
    impl Sealed for super::Installed {}
    impl Sealed for super::Detached {}
    impl Sealed for super::Recovery {}
    impl Sealed for super::Gone {}
}

/// An open game process, not yet identified.
pub struct Candidate<A: Access> {
    game: Box<dyn Game>,
    access: PhantomData<A>,
}

impl<A: Access> Candidate<A> {
    #[cfg_attr(
        not(any(test, windows)),
        expect(dead_code, reason = "only Windows and tests open processes")
    )]
    pub(crate) fn new(game: Box<dyn Game>) -> Self {
        Self {
            game,
            access: PhantomData,
        }
    }

    /// The process id.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.game.pid()
    }
}

impl<A: Access> fmt::Debug for Candidate<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Candidate")
            .field("pid", &self.pid())
            .finish()
    }
}

/// A session phase.
pub trait State: sealed::Sealed {}

/// The process is the supported build; nothing has been changed.
#[derive(Debug)]
pub struct Identified;
/// The trainer owns the process; nothing has been changed.
pub struct Claimed {
    owner: Box<dyn Owner>,
}
/// The caves and the zeroed control word are in the game; no site is hooked.
pub struct Prepared {
    owner: Box<dyn Owner>,
    hooks: Hooks,
}
/// Every site is hooked; publications reach the game.
pub struct Installed {
    owner: Box<dyn Owner>,
    hooks: Hooks,
    revision: u64,
    published: DesiredMask,
}
/// Every site is restored and verified.
pub struct Detached {
    _owner: Box<dyn Owner>,
    report: Restored,
}
/// An install or restore failed; the game may still hold some of the trainer's code.
pub struct Recovery {
    owner: Box<dyn Owner>,
    hooks: Hooks,
}
/// The game exited; nothing remains to restore.
#[derive(Debug)]
pub struct Gone;

impl State for Identified {}
impl State for Claimed {}
impl State for Prepared {}
impl State for Installed {}
impl State for Detached {}
impl State for Recovery {}
impl State for Gone {}

/// Where the trainer's code lives in the game.
struct Hooks {
    controls: Box<dyn Controls>,
    plan: HookPlan,
    journal: PatchJournal,
    /// The cave block while nothing can reach it; `None` once it is retained.
    unpublished: Option<Box<dyn Allocation>>,
    /// Game threads this session suspended that would not resume, each owing one resume.
    stuck: Stuck,
}

impl Hooks {
    /// Every range a frozen thread must not be in: the sites and the caves' code.
    fn ranges(&self) -> Vec<CodeRange> {
        self.plan
            .hooks()
            .iter()
            .flat_map(|hook| [hook.range(), hook.cave().code()])
            .collect()
    }
}

/// What every phase knows about the game.
struct Core {
    game: Box<dyn Game>,
    profile: ValidProfile<'static>,
    image: Va,
    /// The file's base relocations and preferred base, to undo relocations without reading the file again.
    relocations: identify::Relocations,
    brand: u64,
}

/// A game process at one phase.
pub struct Session<S: State, A: Access = Write> {
    core: Core,
    state: S,
    access: PhantomData<A>,
}

impl<S: State, A: Access> fmt::Debug for Session<S, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("pid", &self.pid())
            .field("image", &self.core.image)
            .field("phase", &std::any::type_name::<S>())
            .finish()
    }
}

/// A session of `core` at `state`.
const fn at<T: State, A: Access>(core: Core, state: T) -> Session<T, A> {
    Session {
        core,
        state,
        access: PhantomData,
    }
}

impl<S: State, A: Access> Session<S, A> {
    /// The process id.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.core.game.pid()
    }

    /// Where the game image is loaded.
    #[must_use]
    pub const fn image(&self) -> Va {
        self.core.image
    }

    /// The profile it was identified with.
    #[must_use]
    pub const fn profile(&self) -> &ValidProfile<'static> {
        &self.core.profile
    }

    /// Proof that the game has exited, if it has.
    #[must_use]
    pub fn exit_proof(&self) -> Option<ProcessExited> {
        self.core.game.exited().then_some(ProcessExited {
            brand: self.core.brand,
        })
    }

    /// Ends the session of a game that exited. Nothing is read from or written to the game; local resources
    /// are released.
    ///
    /// # Errors
    ///
    /// Gives the session back if `proof` is for another session.
    pub fn exited(self, proof: ProcessExited) -> Result<Session<Gone, A>, Self> {
        if proof.brand != self.core.brand {
            return Err(self);
        }
        Ok(at(self.core, Gone))
    }
}

/// Proof that a session's game exited, from [`Session::exit_proof`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessExited {
    brand: u64,
}

/// A transition that failed, with the session in the phase it fell back to.
pub struct TransitionFailure<S: State, A: Access = Write> {
    /// The session.
    pub session: Session<S, A>,
    /// What failed.
    pub error: TrainerError,
}

impl<S: State, A: Access> fmt::Debug for TransitionFailure<S, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransitionFailure")
            .field("session", &self.session)
            .field("error", &self.error)
            .finish()
    }
}

/// An identification that failed, with the still-open process to try again.
pub struct Rejected<A: Access> {
    /// The process.
    pub candidate: Candidate<A>,
    /// Why it is not the supported build, or not yet.
    pub error: IdentifyError,
}

impl<A: Access> fmt::Debug for Rejected<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rejected")
            .field("candidate", &self.candidate)
            .field("error", &self.error)
            .finish()
    }
}

static BRANDS: AtomicU64 = AtomicU64::new(1);

/// Proves `candidate` is the build `profile` supports, without changing it.
///
/// # Errors
///
/// [`Rejected`] with the candidate and the first failed check. While the game is still unpacking its code,
/// [`IdentifyError::transient`] is true and trying again later may succeed.
pub fn identify<A: Access>(
    candidate: Candidate<A>,
    profile: &ValidProfile<'static>,
) -> Result<Session<Identified, A>, Rejected<A>> {
    match identify::identify(&*candidate.game, profile) {
        Ok((image, relocations)) => Ok(at(
            Core {
                game: candidate.game,
                profile: profile.clone(),
                image,
                relocations,
                brand: BRANDS.fetch_add(1, Ordering::Relaxed),
            },
            Identified,
        )),
        Err(error) => Err(Rejected { candidate, error }),
    }
}

impl Session<Identified, Write> {
    /// Takes this game's owner and stop objects.
    ///
    /// # Errors
    ///
    /// The session back, with [`TrainerError::Claim`].
    pub fn claim(self) -> Result<Session<Claimed>, Box<TransitionFailure<Identified>>> {
        match self.core.game.claim(self.core.profile.profile().slug) {
            Ok(owner) => Ok(at(self.core, Claimed { owner })),
            Err(error) => Err(Box::new(TransitionFailure {
                session: self,
                error: TrainerError::Claim(error),
            })),
        }
    }
}

impl Session<Claimed> {
    /// Maps the zeroed control word and writes every cave, protected and checked, where every site can reach
    /// it.
    ///
    /// # Errors
    ///
    /// The session back, with [`TrainerError::Prepare`]. Whatever was allocated is freed: no site jumps to it.
    pub fn prepare(self) -> Result<Session<Prepared>, Box<TransitionFailure<Claimed>>> {
        let core = &self.core;
        match prepare::prepare(&*core.game, &core.profile, core.image) {
            Ok(hooks) => Ok(at(
                self.core,
                Prepared {
                    owner: self.state.owner,
                    hooks,
                },
            )),
            Err(error) => Err(Box::new(TransitionFailure {
                session: self,
                error: TrainerError::Prepare(error),
            })),
        }
    }
}

/// How long a code change held the game.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timing {
    /// Freeze attempts, including the one that held.
    pub attempts: u32,
    /// From the first attempt until the game was frozen.
    pub acquire: Duration,
    /// How long the game stayed frozen.
    pub held: Duration,
}

/// What a frozen code change did.
struct Changed {
    outcome: Result<(), TrainerError>,
    timing: Timing,
    text: TextCheck,
}

/// What a restore checks besides the sites, which it always reads back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VerifyRestore {
    /// Only the restored sites.
    #[default]
    Sites,
    /// Also hashes the whole code section while the game is still frozen and compares it with the
    /// supported build.
    Text,
}

/// The whole-code check of a restore.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextCheck {
    /// The code section matches the supported build.
    Identical,
    /// The code section differs from the supported build, though every site restored. Something else changed
    /// the game's code.
    Differs,
    /// Not checked.
    Skipped(SkipReason),
}

/// Why a restore did not hash the code section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// It was not asked for.
    NotRequested,
    /// Restoration was incomplete.
    Incomplete,
    /// The code section could not be read.
    Unreadable,
    /// No site was ever written, so there was nothing to restore or check.
    Untouched,
}

/// A completed restoration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Restored {
    /// How long it held the game.
    pub timing: Timing,
    /// The whole-code check.
    pub text: TextCheck,
}

type Step<'a> = &'a mut dyn FnMut(&Freeze<'_>, &mut Hooks) -> Result<(), TrainerError>;

/// Freezes the game, runs `change` with the threads held, retries `recover` while it fails, and resumes.
/// `settle` says what a failed change amounts to once `recover` succeeded.
///
/// Freezing is retried while it may succeed, until `stop` says to give up or the game exits. A panic in
/// `change` or `recover` is caught; the journal they share stays usable because ownership is recorded before
/// each write. With [`VerifyRestore::Text`], a successful change is followed by a hash of the whole code
/// section before the game resumes.
fn frozen_change(
    core: &Core,
    hooks: &mut Hooks,
    stop: &dyn Fn() -> bool,
    change: Step<'_>,
    recover: Step<'_>,
    settle: fn(Result<(), TrainerError>) -> Result<(), TrainerError>,
    verify: VerifyRestore,
) -> Changed {
    let game = &*core.game;
    let ranges = hooks.ranges();
    let started = Instant::now();
    let mut attempts = 0;
    let not_held = |attempts, outcome| Changed {
        outcome: Err(outcome),
        timing: Timing {
            attempts,
            acquire: started.elapsed(),
            held: Duration::ZERO,
        },
        text: TextCheck::Skipped(SkipReason::Incomplete),
    };
    hooks.stuck.retry();
    // The whole-code check reads into a buffer allocated before the game is frozen.
    let mut text_buffer = match verify {
        VerifyRestore::Text => vec![0; core.profile.profile().identity.text.size as usize],
        VerifyRestore::Sites => Vec::new(),
    };
    let frozen = loop {
        attempts += 1;
        match catch_unwind(AssertUnwindSafe(|| Freeze::new(game, &ranges))) {
            Ok(Ok(frozen)) => break frozen,
            Ok(Err(failure)) => {
                let transient = failure.error.transient();
                hooks.stuck.absorb(failure.stuck);
                if game.exited() && hooks.stuck.is_empty() {
                    return not_held(attempts, TrainerError::Exited);
                }
                if !transient || attempts >= FREEZE_ATTEMPTS {
                    return not_held(attempts, TrainerError::Freeze(failure.error));
                }
            }
            Err(_) => return not_held(attempts, TrainerError::Panicked),
        }
        game.wait(RETRY_WAIT);
        if game.exited() {
            return not_held(attempts, TrainerError::Exited);
        }
        if stop() {
            return not_held(attempts, TrainerError::Stopped);
        }
    };
    let acquire = started.elapsed();
    let held = Instant::now();
    let caught = |result: std::thread::Result<Result<(), TrainerError>>| {
        result.unwrap_or(Err(TrainerError::Panicked))
    };
    let mut outcome = caught(catch_unwind(AssertUnwindSafe(|| change(&frozen, hooks))));
    let mut last = None;
    for attempt in 0..ROLLBACK_ATTEMPTS {
        if outcome.is_ok() || hooks.journal.is_clean() || game.exited() {
            break;
        }
        if attempt > 0 {
            game.wait(RETRY_WAIT);
        }
        match caught(catch_unwind(AssertUnwindSafe(|| recover(&frozen, hooks)))) {
            Ok(()) => {
                last = None;
                break;
            }
            Err(error) => last = Some(error),
        }
    }
    if outcome.is_err() && hooks.journal.is_clean() {
        outcome = settle(outcome);
    } else if let (Err(TrainerError::Panicked), Some(left @ TrainerError::Restore(_))) =
        (&outcome, last)
    {
        // A panic whose rollback failed too leaves the game holding the trainer's code: say that.
        outcome = Err(left);
    }
    if outcome.is_err() && game.exited() {
        // A game that died mid-change cannot be read or written; nothing of it remains to restore.
        outcome = Err(TrainerError::Exited);
    }
    let text = match (verify, &outcome) {
        (VerifyRestore::Sites, _) => TextCheck::Skipped(SkipReason::NotRequested),
        (VerifyRestore::Text, Err(_)) => TextCheck::Skipped(SkipReason::Incomplete),
        (VerifyRestore::Text, Ok(())) => text_check(core, &mut text_buffer),
    };
    let resumed = frozen.resume();
    let timing = Timing {
        attempts,
        acquire,
        held: held.elapsed(),
    };
    // Threads left suspended matter more than anything else that failed: the game is stuck until they resume.
    // Including those owed from earlier freezes that would not resume again.
    if let Err(stuck) = resumed {
        hooks.stuck.absorb(stuck);
    }
    if !hooks.stuck.is_empty() {
        outcome = Err(TrainerError::Resume(ResumeError {
            failed: hooks.stuck.len(),
        }));
    }
    Changed {
        outcome,
        timing,
        text,
    }
}

/// Hashes the code section, relocations undone, and compares it with the supported build.
fn text_check(core: &Core, text: &mut [u8]) -> TextCheck {
    let section = core.profile.profile().identity.text;
    let read = core
        .image
        .at(section.rva)
        .ok()
        .and_then(|at| core.game.read(at, text).ok())
        .and_then(|()| core.relocations.undo(text, section.rva, core.image).ok());
    match read {
        Some(()) if Sha256Digest::of(text) == section.sha256 => TextCheck::Identical,
        Some(()) => TextCheck::Differs,
        None => TextCheck::Skipped(SkipReason::Unreadable),
    }
}

impl Session<Prepared> {
    /// Freezes the game and writes every site's jump, with every cheat off.
    ///
    /// Freezing is retried for about a second while a game thread is inside code about to change, or the
    /// thread list keeps changing; a stop request ends the wait. The cave block is retained before the first
    /// write.
    ///
    /// # Errors
    ///
    /// A [`Recovery`] session with the failure; [`Session::restore`] it. If nothing was written it restores at
    /// once.
    pub fn install(self) -> Result<Session<Installed>, Box<TransitionFailure<Recovery>>> {
        let Prepared { owner, mut hooks } = self.state;
        let changed = frozen_change(
            &self.core,
            &mut hooks,
            &|| owner.stop_requested() || self.core.game.interrupted(),
            &mut |frozen, hooks| {
                let Hooks {
                    journal,
                    unpublished,
                    ..
                } = hooks;
                // From the first write on, a game thread may run the caves: give them up for good.
                let mut retain = || {
                    if let Some(block) = unpublished.take() {
                        block.retain();
                    }
                };
                patch::apply(frozen, journal, &mut retain).map_err(TrainerError::Apply)
            },
            &mut |frozen, hooks| {
                patch::restore(frozen, &mut hooks.journal).map_err(TrainerError::Restore)
            },
            // A rollback that succeeded on a retry leaves nothing behind.
            |outcome| match outcome {
                Err(TrainerError::Apply(mut error)) => {
                    error.rollback = Ok(());
                    Err(TrainerError::Apply(error))
                }
                other => other,
            },
            VerifyRestore::Sites,
        );
        match changed.outcome {
            Ok(()) => Ok(at(
                self.core,
                Installed {
                    owner,
                    hooks,
                    revision: 0,
                    published: DesiredMask::ZERO,
                },
            )),
            Err(error) => Err(Box::new(TransitionFailure {
                session: at(self.core, Recovery { owner, hooks }),
                error,
            })),
        }
    }
}

impl Session<Installed> {
    /// Stores `mask` in the control word, unless it is already there, and acknowledges it.
    ///
    /// This is one local interlocked store. It never freezes the game or touches code.
    pub fn publish(&mut self, mask: DesiredMask) -> Ack {
        let state = &mut self.state;
        if mask != state.published {
            state.hooks.controls.store(mask.bits().word());
            state.published = mask;
        }
        state.revision += 1;
        Ack::new(state.revision, ControlMask::published(mask))
    }

    /// The mask the game sees.
    #[must_use]
    pub const fn published(&self) -> DesiredMask {
        self.state.published
    }

    /// Whether someone asked this trainer to stop.
    #[must_use]
    pub fn stop_requested(&self) -> bool {
        self.state.owner.stop_requested()
    }

    /// Publishes zero, then freezes the game and restores every site, checking each.
    ///
    /// # Errors
    ///
    /// A [`Recovery`] session naming every site still hooked; [`TrainerError::Exited`] if the game exited
    /// first, so nothing could be restored.
    pub fn restore(
        self,
        verify: VerifyRestore,
    ) -> Result<Session<Detached>, Box<TransitionFailure<Recovery>>> {
        let Installed { owner, hooks, .. } = self.state;
        at(self.core, Recovery { owner, hooks }).restore(verify)
    }
}

impl Session<Recovery> {
    /// The sites the game may still hold the trainer's jumps in.
    #[must_use]
    pub fn hooked(&self) -> Vec<SiteId> {
        self.state.hooks.journal.owned()
    }

    /// Protection changes not yet undone.
    #[must_use]
    pub fn protection_debt(&self) -> usize {
        self.state.hooks.journal.debt()
    }

    /// Game threads this session suspended that would not resume. Restoring tries them again.
    #[must_use]
    pub fn suspended_threads(&self) -> usize {
        self.state.hooks.stuck.len()
    }

    /// Publishes zero, then freezes the game and restores every site still hooked, checking each.
    ///
    /// # Errors
    ///
    /// The session back, naming every site still hooked; [`TrainerError::Exited`] if the game exited, so
    /// nothing could be restored.
    pub fn restore(
        mut self,
        verify: VerifyRestore,
    ) -> Result<Session<Detached>, Box<TransitionFailure<Recovery>>> {
        let hooks = &mut self.state.hooks;
        hooks.controls.store(0);
        let changed = if self.core.game.exited() {
            Changed {
                outcome: Err(TrainerError::Exited),
                timing: Timing::default(),
                text: TextCheck::Skipped(SkipReason::Incomplete),
            }
        } else if hooks.journal.is_clean() {
            hooks.stuck.retry();
            Changed {
                outcome: if hooks.stuck.is_empty() {
                    Ok(())
                } else {
                    Err(TrainerError::Resume(ResumeError {
                        failed: hooks.stuck.len(),
                    }))
                },
                timing: Timing::default(),
                text: TextCheck::Skipped(match verify {
                    VerifyRestore::Sites => SkipReason::NotRequested,
                    VerifyRestore::Text => SkipReason::Untouched,
                }),
            }
        } else {
            let restore: Step<'_> = &mut |frozen, hooks| {
                patch::restore(frozen, &mut hooks.journal).map_err(TrainerError::Restore)
            };
            let mut again = |frozen: &Freeze<'_>, hooks: &mut Hooks| {
                patch::restore(frozen, &mut hooks.journal).map_err(TrainerError::Restore)
            };
            frozen_change(
                &self.core,
                hooks,
                &|| false,
                restore,
                &mut again,
                // Restoring again is the same restoration, succeeding.
                |_| Ok(()),
                verify,
            )
        };
        hooks.controls.finished();
        match changed.outcome {
            Ok(()) => {
                let Recovery { owner, .. } = self.state;
                Ok(at(
                    self.core,
                    Detached {
                        _owner: owner,
                        report: Restored {
                            timing: changed.timing,
                            text: changed.text,
                        },
                    },
                ))
            }
            Err(error) => Err(Box::new(TransitionFailure {
                session: self,
                error,
            })),
        }
    }
}

impl Session<Detached> {
    /// What the restoration did.
    #[must_use]
    pub const fn report(&self) -> Restored {
        self.state.report
    }
}

/// What asking one game's trainer to stop did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopOutcome {
    /// The trainer's stop event was set. Whether it restored the game is in its own log and exit code.
    Requested,
    /// No trainer owns that game.
    NoTrainer,
    /// The stop event could not be opened or set.
    Failed(OsError),
}

/// Why a game could not be claimed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClaimError {
    /// Another trainer owns this game process.
    #[error("another trainer already owns this game")]
    Owned,
    /// The owner objects could not be created.
    #[error("cannot create the trainer's owner objects: {0}")]
    Os(OsError),
}

impl ClaimError {
    /// The one next step to offer.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        match self {
            Self::Owned => RecoveryAction::CloseOtherTrainer,
            Self::Os(_) => RecoveryAction::Retry,
        }
    }
}

/// Why a session transition failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TrainerError {
    /// The game is not the supported build.
    #[error(transparent)]
    Identify(#[from] IdentifyError),
    /// The game could not be claimed.
    #[error(transparent)]
    Claim(ClaimError),
    /// The caves or control word could not be prepared.
    #[error(transparent)]
    Prepare(PrepareError),
    /// The game could not be frozen.
    #[error(transparent)]
    Freeze(FreezeError),
    /// The hooks could not be written.
    #[error(transparent)]
    Apply(ApplyError),
    /// Some of the trainer's changes are still in the game.
    #[error(transparent)]
    Restore(RestoreIncomplete),
    /// Game threads could not be resumed.
    #[error(transparent)]
    Resume(ResumeError),
    /// A code change panicked; what it owned was rolled back where possible.
    #[error("the trainer failed while changing the game's code")]
    Panicked,
    /// A stop request ended the wait to freeze the game; nothing was written.
    #[error("stopped before the hooks were written")]
    Stopped,
    /// The game exited.
    #[error("the game exited")]
    Exited,
}

impl TrainerError {
    /// The one next step to offer.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        match self {
            Self::Identify(error) => error.recovery(),
            Self::Claim(error) => error.recovery(),
            Self::Prepare(error) => error.recovery(),
            Self::Freeze(error) => error.recovery(),
            Self::Apply(error) => match &error.rollback {
                Ok(()) => error.cause.recovery(),
                Err(left) => left.recovery(),
            },
            Self::Restore(error) => error.recovery(),
            Self::Resume(_) => RecoveryAction::RestartGame,
            Self::Panicked => RecoveryAction::ReportBug,
            Self::Stopped => RecoveryAction::Retry,
            Self::Exited => RecoveryAction::StartGame,
        }
    }
}
