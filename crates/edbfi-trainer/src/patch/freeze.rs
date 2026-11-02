// SPDX-License-Identifier: AGPL-3.0-only
//! Holding every game thread still while code changes.
//!
//! [`Freeze::new`] enumerates the game's threads until the sorted id set comes out the same three times in a
//! row, at most [`Freeze::PASSES`] times. Every new thread is opened, checked to still belong to the game,
//! and suspended, with each suspension recorded so it is undone exactly once; a thread that was already
//! suspended stays suspended. Then each thread's instruction pointer must lie outside every range about to
//! change. Any failure resumes what it suspended and reports both.
#![forbid(unsafe_code)]

use std::fmt;

use crate::addr::{CodeRange, Va};
use crate::recovery::RecoveryAction;

use super::{OsError, Process, Thread};

/// The game's threads, suspended. Only a `Freeze` can write code.
///
/// [`Freeze::resume`] resumes them and reports any thread that would not resume. Dropping a freeze resumes
/// them too, ignoring failures, so an early return or a panic never leaves the game suspended.
pub(crate) struct Freeze<'p> {
    process: &'p dyn Process,
    suspended: Vec<Box<dyn Thread>>,
}

impl fmt::Debug for Freeze<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Freeze")
            .field("pid", &self.process.pid())
            .field("suspended", &self.suspended.len())
            .finish()
    }
}

impl<'p> Freeze<'p> {
    /// Most enumeration passes.
    pub(crate) const PASSES: usize = 16;

    /// Suspends every thread of `process`, then checks none is executing inside `ranges`.
    ///
    /// # Errors
    ///
    /// A [`FreezeError`] naming the failure, after resuming every thread this call suspended.
    pub(crate) fn new(process: &'p dyn Process, ranges: &[CodeRange]) -> Result<Self, FreezeError> {
        let mut freeze = Self {
            process,
            suspended: Vec::new(),
        };
        match freeze.suspend_all().and_then(|()| freeze.check(ranges)) {
            Ok(()) => Ok(freeze),
            Err(kind) => Err(FreezeError {
                kind,
                unresumed: freeze.resume_all(),
            }),
        }
    }

    /// The frozen process.
    pub(crate) fn process(&self) -> &'p dyn Process {
        self.process
    }

    /// How many threads it holds.
    #[cfg(test)]
    pub(crate) fn threads(&self) -> usize {
        self.suspended.len()
    }

    fn suspend_all(&mut self) -> Result<(), FreezeKind> {
        let pid = self.process.pid();
        let mut known: Vec<u32> = Vec::new();
        let mut seen: Vec<u32> = Vec::new();
        let mut repeats = 0;
        for _ in 0..Self::PASSES {
            let mut ids = self.process.threads().map_err(FreezeKind::Enumerate)?;
            ids.sort_unstable();
            ids.dedup();
            if ids.is_empty() {
                return Err(FreezeKind::NoThreads);
            }
            for &id in &ids {
                if seen.binary_search(&id).is_ok() {
                    continue;
                }
                let thread = self
                    .process
                    .open_thread(id)
                    .map_err(|source| FreezeKind::Open { thread: id, source })?;
                match thread.process_id() {
                    Ok(owner) if owner == pid => {}
                    Ok(_) => return Err(FreezeKind::Owner { thread: id }),
                    Err(source) => return Err(FreezeKind::Open { thread: id, source }),
                }
                thread
                    .suspend()
                    .map_err(|source| FreezeKind::Suspend { thread: id, source })?;
                self.suspended.push(thread);
                if let Err(at) = seen.binary_search(&id) {
                    seen.insert(at, id);
                }
            }
            repeats = if ids == known { repeats + 1 } else { 0 };
            known = ids;
            if repeats == 2 {
                return Ok(());
            }
        }
        Err(FreezeKind::Unstable)
    }

    fn check(&self, ranges: &[CodeRange]) -> Result<(), FreezeKind> {
        for thread in &self.suspended {
            let rip = thread.rip().map_err(FreezeKind::Context)?;
            if let Some(&range) = ranges.iter().find(|range| range.contains(rip)) {
                return Err(FreezeKind::Busy { rip, range });
            }
        }
        Ok(())
    }

    /// Resumes every held thread once, newest first, returning how many failed.
    fn resume_all(&mut self) -> usize {
        let mut failed = 0;
        while let Some(thread) = self.suspended.pop() {
            if thread.resume().is_err() {
                failed += 1;
            }
        }
        failed
    }

    /// Resumes every thread.
    ///
    /// # Errors
    ///
    /// [`ResumeError`] with the number of threads that would not resume; they stay suspended.
    pub(crate) fn resume(mut self) -> Result<(), ResumeError> {
        match self.resume_all() {
            0 => Ok(()),
            failed => Err(ResumeError { failed }),
        }
    }
}

impl Drop for Freeze<'_> {
    fn drop(&mut self) {
        let _failed = self.resume_all();
    }
}

/// Why the game could not be frozen.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FreezeKind {
    /// The thread list could not be read completely.
    #[error("cannot enumerate the game's threads: {0}")]
    Enumerate(OsError),
    /// The game has no threads.
    #[error("the game has no threads")]
    NoThreads,
    /// The thread list kept changing.
    #[error("the game's threads kept changing")]
    Unstable,
    /// A thread could not be opened.
    #[error("cannot open game thread {thread}: {source}")]
    Open {
        /// The thread id.
        thread: u32,
        /// The failed call.
        source: OsError,
    },
    /// A thread id now belongs to another process.
    #[error("thread {thread} no longer belongs to the game")]
    Owner {
        /// The thread id.
        thread: u32,
    },
    /// A thread could not be suspended.
    #[error("cannot suspend game thread {thread}: {source}")]
    Suspend {
        /// The thread id.
        thread: u32,
        /// The failed call.
        source: OsError,
    },
    /// A suspended thread's position could not be read.
    #[error("cannot read a game thread's position: {0}")]
    Context(OsError),
    /// A thread is executing code about to change.
    #[error("a game thread is at {rip}, inside code about to change")]
    Busy {
        /// Where the thread is.
        rip: Va,
        /// The range it is in.
        range: CodeRange,
    },
}

/// A freeze that failed, and the threads it could not resume afterwards.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{kind}{}", if *unresumed > 0 { format!("; {unresumed} thread(s) could not be resumed") } else { String::new() })]
pub struct FreezeError {
    /// What failed.
    pub kind: FreezeKind,
    /// Threads left suspended.
    pub unresumed: usize,
}

impl FreezeError {
    /// Whether trying again may succeed: a busy or changing thread moves on; a left-suspended thread or a
    /// foreign owner does not get better by waiting.
    #[must_use]
    pub const fn transient(&self) -> bool {
        self.unresumed == 0 && !matches!(self.kind, FreezeKind::NoThreads)
    }

    /// The one next step to offer.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        if self.unresumed > 0 {
            RecoveryAction::RestartGame
        } else {
            RecoveryAction::Retry
        }
    }
}

/// Threads that would not resume.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{failed} game thread(s) could not be resumed")]
pub struct ResumeError {
    /// How many.
    pub failed: usize,
}
