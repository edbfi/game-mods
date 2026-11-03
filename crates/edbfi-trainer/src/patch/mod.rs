// SPDX-License-Identifier: AGPL-3.0-only
//! Code patches in another process, and the record of what the trainer owns there.
//!
//! Everything here works through `Process`, a private seam the Windows backend implements and the tests fake,
//! so every failure of every call can be injected.
//!
//! - `write_code` changes executable bytes only while the game is frozen. It first undoes any protection
//!   change left from an earlier failure, makes each committed region writable while recording its previous
//!   protection, writes, flushes and restores the protections in reverse. It succeeds only if all of that
//!   does.
//! - `apply` checks every original first, then patches in order. It checks each original again just before
//!   writing it, and records the patch as owned before the write, so a write that fails halfway is still
//!   owned. Any failure rolls back everything it owns.
//! - `restore` goes over the owned patches in reverse. A patch whose bytes are each either original or
//!   replacement is the trainer's; anything else is foreign and is left alone, still owned. Every patch that
//!   can be restored is, whatever happens to the others.
//!
//! The `PatchJournal` keeps ownership and protection debt. As long as it holds either, the game may still
//! hold the trainer's code, and the session must not publish anything but zero or attach again.
#![forbid(unsafe_code)]

pub(crate) mod freeze;

#[cfg(test)]
pub(crate) mod fake;
#[cfg(test)]
mod tests;

use std::fmt;

use crate::addr::{CodeRange, Va};
use crate::profile::SiteId;
use crate::recovery::RecoveryAction;

pub(crate) use freeze::{Freeze, Stuck};
pub use freeze::{FreezeError, FreezeKind, ResumeError};

/// A Windows memory protection, one of the `PAGE_*` values.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Protection(u32);

impl Protection {
    /// `PAGE_NOACCESS`.
    pub const NO_ACCESS: Self = Self(0x01);
    /// `PAGE_READWRITE`.
    pub const READ_WRITE: Self = Self(0x04);
    /// `PAGE_EXECUTE_READ`.
    pub const EXECUTE_READ: Self = Self(0x20);
    /// `PAGE_EXECUTE_READWRITE`.
    pub const EXECUTE_READ_WRITE: Self = Self(0x40);
    /// `PAGE_GUARD`, a modifier.
    pub const GUARD: Self = Self(0x100);

    /// The protection with the raw value `value`.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// The raw value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Whether memory with this protection can be read without a fault: not a guard page, not no-access.
    #[must_use]
    pub const fn accessible(self) -> bool {
        self.0 != 0 && self.0 & (Self::GUARD.0 | Self::NO_ACCESS.0) == 0
    }
}

impl fmt::Debug for Protection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Protection({:#x})", self.0)
    }
}

/// A failed operating-system call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, thiserror::Error)]
#[error("{call} failed (OS error {code})")]
pub struct OsError {
    /// The Win32 function.
    pub call: &'static str,
    /// Its `GetLastError` code.
    pub code: u32,
}

impl OsError {
    /// `call` failed with `code`.
    #[must_use]
    pub const fn new(call: &'static str, code: u32) -> Self {
        Self { call, code }
    }
}

/// What the address space holds at an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegionState {
    /// Nothing is reserved there.
    Free,
    /// Reserved, without backing memory.
    #[cfg_attr(
        not(any(test, windows)),
        expect(dead_code, reason = "only Windows and tests report it")
    )]
    Reserved,
    /// Committed memory.
    Committed,
}

/// A run of pages with the same state and protection, as `VirtualQueryEx` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    /// Its first byte.
    pub(crate) base: Va,
    /// Its length in bytes.
    pub(crate) size: u64,
    /// Its state.
    pub(crate) state: RegionState,
    /// Its protection, meaningful when committed.
    pub(crate) protect: Protection,
}

impl Region {
    /// One past its last byte, or `None` past the address space.
    pub(crate) fn end(self) -> Option<u64> {
        self.base.get().checked_add(self.size)
    }
}

/// The calls the patch engine makes on a game process. The Windows backend implements it over a process
/// handle; tests implement it over a byte map that can fail any call.
pub(crate) trait Process {
    /// The process id.
    fn pid(&self) -> u32;
    /// Reads exactly `into.len()` bytes at `at`; a short read is an error.
    fn read(&self, at: Va, into: &mut [u8]) -> Result<(), OsError>;
    /// The region containing `at`.
    fn query(&self, at: Va) -> Result<Region, OsError>;
    /// Sets the protection of `[at, at + len)`, returning the previous protection of its first page.
    fn protect(&self, at: Va, len: u64, to: Protection) -> Result<Protection, OsError>;
    /// Writes `bytes` at `at`. On failure, any subset of the bytes may have changed.
    fn write(&self, at: Va, bytes: &[u8]) -> Result<(), OsError>;
    /// Flushes the instruction cache for `[at, at + len)`.
    fn flush(&self, at: Va, len: u64) -> Result<(), OsError>;
    /// The ids of the process's threads, from one complete enumeration.
    fn threads(&self) -> Result<Vec<u32>, OsError>;
    /// Opens thread `id` to suspend, resume and inspect it.
    fn open_thread(&self, id: u32) -> Result<Box<dyn Thread>, OsError>;
}

/// An open thread of the game.
pub(crate) trait Thread {
    /// The id of the process that owns it now.
    fn process_id(&self) -> Result<u32, OsError>;
    /// Suspends it, returning its previous suspend count.
    fn suspend(&self) -> Result<u32, OsError>;
    /// Resumes it once, returning its previous suspend count.
    fn resume(&self) -> Result<u32, OsError>;
    /// The instruction pointer of the suspended thread.
    fn rip(&self) -> Result<Va, OsError>;
}

/// One hook site's patch: the jump written over its original bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Patch {
    pub(crate) site: SiteId,
    pub(crate) range: CodeRange,
    pub(crate) original: Vec<u8>,
    pub(crate) replacement: Vec<u8>,
}

impl Patch {
    fn at(&self) -> Va {
        self.range.start()
    }
}

/// A protection the trainer changed and has not yet put back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Debt {
    at: Va,
    len: u64,
    previous: Protection,
}

/// What the trainer owns in the game: which patches may be written, and which protections it changed.
#[derive(Debug)]
pub(crate) struct PatchJournal {
    patches: Vec<Patch>,
    owned: Vec<bool>,
    debt: Vec<Debt>,
}

impl PatchJournal {
    /// A journal for `patches`, owning none of them.
    ///
    /// # Errors
    ///
    /// [`PatchError::Invalid`] if a patch's original and replacement differ in length or match its range,
    /// or two patches overlap. A verified profile never does this.
    pub(crate) fn new(patches: Vec<Patch>) -> Result<Self, PatchError> {
        for (i, patch) in patches.iter().enumerate() {
            let len = patch.range.len().get() as usize;
            if patch.original.len() != len || patch.replacement.len() != len {
                return Err(PatchError::Invalid { site: patch.site });
            }
            if patches[..i]
                .iter()
                .any(|other| other.range.overlaps(patch.range))
            {
                return Err(PatchError::Invalid { site: patch.site });
            }
        }
        let owned = vec![false; patches.len()];
        Ok(Self {
            patches,
            owned,
            debt: Vec::new(),
        })
    }

    /// The patched ranges, in order.
    #[cfg(test)]
    pub(crate) fn ranges(&self) -> impl Iterator<Item = CodeRange> {
        self.patches.iter().map(|patch| patch.range)
    }

    /// The sites the trainer may have written, in order.
    pub(crate) fn owned(&self) -> Vec<SiteId> {
        self.patches
            .iter()
            .zip(&self.owned)
            .filter_map(|(patch, &owned)| owned.then_some(patch.site))
            .collect()
    }

    /// Whether any patch is owned.
    pub(crate) fn owns_any(&self) -> bool {
        self.owned.contains(&true)
    }

    /// The number of protection changes not yet undone.
    pub(crate) fn debt(&self) -> usize {
        self.debt.len()
    }

    /// Whether the game holds nothing of the trainer's: no owned patch and no changed protection.
    pub(crate) fn is_clean(&self) -> bool {
        !self.owns_any() && self.debt.is_empty()
    }
}

/// Undoes every recorded protection change it can, newest first. `Ok` once none remain.
fn settle(process: &dyn Process, debt: &mut Vec<Debt>) -> Result<(), MemoryError> {
    let mut first = None;
    let mut i = debt.len();
    while i > 0 {
        i -= 1;
        let entry = debt[i];
        match process.protect(entry.at, entry.len, entry.previous) {
            Ok(_) => {
                debt.remove(i);
            }
            Err(source) => {
                first.get_or_insert(MemoryError::Reprotect {
                    at: entry.at,
                    source,
                });
            }
        }
    }
    first.map_or(Ok(()), Err)
}

/// Writes `bytes` over executable code at `at`, while `frozen` holds the game's threads.
///
/// Protection debt from earlier writes is settled first; a write never starts while any remains. Each region
/// in the range must be committed and accessible. Its previous protection is recorded before it is made
/// writable, so a failure at any step leaves debt that a later call settles. Success needs the whole write,
/// the flush and every protection put back.
pub(crate) fn write_code(
    frozen: &Freeze<'_>,
    journal: &mut PatchJournal,
    at: Va,
    bytes: &[u8],
) -> Result<(), MemoryError> {
    let process = frozen.process();
    let debt = &mut journal.debt;
    settle(process, debt)?;
    let len = bytes.len() as u64;
    let end = at
        .get()
        .checked_add(len)
        .filter(|_| len > 0)
        .ok_or(MemoryError::Inaccessible { at })?;
    let mut cursor = at.get();
    while cursor < end {
        let here = Va::new(cursor);
        let step = match process.query(here) {
            Ok(region) => region
                .end()
                .filter(|&region_end| {
                    region.state == RegionState::Committed
                        && region.protect.accessible()
                        && region.base.get() <= cursor
                        && region_end > cursor
                })
                .map(|region_end| region_end.min(end) - cursor)
                .ok_or(MemoryError::Inaccessible { at: here }),
            Err(source) => Err(MemoryError::Query { at: here, source }),
        };
        let changed = step.and_then(|step| {
            process
                .protect(here, step, Protection::EXECUTE_READ_WRITE)
                .map(|previous| (step, previous))
                .map_err(|source| MemoryError::Unprotect { at: here, source })
        });
        match changed {
            Ok((step, previous)) => {
                debt.push(Debt {
                    at: here,
                    len: step,
                    previous,
                });
                cursor += step;
            }
            Err(error) => {
                let _settled = settle(process, debt);
                return Err(error);
            }
        }
    }
    let written = process
        .write(at, bytes)
        .map_err(|source| MemoryError::Write { at, len, source });
    let flushed = process
        .flush(at, len)
        .map_err(|source| MemoryError::Flush { at, source });
    let settled = settle(process, debt);
    written.and(flushed).and(settled)
}

/// Whether `at` reads exactly `expected`.
fn read_back(process: &dyn Process, at: Va, expected: &[u8]) -> Result<(), MemoryError> {
    let mut found = vec![0; expected.len()];
    process
        .read(at, &mut found)
        .map_err(|source| MemoryError::Read {
            at,
            len: expected.len(),
            source,
        })?;
    if found == expected {
        Ok(())
    } else {
        Err(MemoryError::Readback { at })
    }
}

/// Writes every patch of `journal`, while `frozen` holds the game's threads.
///
/// Every original is checked before any write and again just before its own. `first_write` runs once,
/// after every original is checked and before anything is written. A patch is owned from just before its
/// write. On any failure everything owned is restored, and the result reports both.
pub(crate) fn apply(
    frozen: &Freeze<'_>,
    journal: &mut PatchJournal,
    first_write: &mut dyn FnMut(),
) -> Result<(), ApplyError> {
    let process = frozen.process();
    if journal.owns_any() {
        return Err(ApplyError {
            cause: PatchError::AlreadyOwned,
            rollback: Ok(()),
        });
    }
    let original = |patch: &Patch| match read_back(process, patch.at(), &patch.original) {
        Ok(()) => Ok(()),
        Err(MemoryError::Readback { .. }) => Err(PatchError::Conflict { site: patch.site }),
        Err(source) => Err(PatchError::Memory {
            site: patch.site,
            source,
        }),
    };
    if let Some(cause) = journal
        .patches
        .iter()
        .find_map(|patch| original(patch).err())
    {
        return Err(ApplyError {
            cause,
            rollback: Ok(()),
        });
    }
    first_write();
    for i in 0..journal.patches.len() {
        let patch = &journal.patches[i];
        let (site, at, replacement) = (patch.site, patch.at(), patch.replacement.clone());
        let step = original(patch).and_then(|()| {
            journal.owned[i] = true;
            write_code(frozen, journal, at, &replacement)
                .and_then(|()| read_back(process, at, &replacement))
                .map_err(|source| PatchError::Memory { site, source })
        });
        if let Err(cause) = step {
            return Err(ApplyError {
                cause,
                rollback: restore(frozen, journal),
            });
        }
    }
    Ok(())
}

/// Restores every owned patch it can, newest first, while `frozen` holds the game's threads.
///
/// A patch whose current bytes are each its original or its replacement byte is rewritten to the original
/// and checked; once it reads back, it is no longer owned. Any other byte is foreign: the patch stays owned
/// and is not written. Every independent patch is tried whatever happens to the others, and debt is settled.
///
/// # Errors
///
/// [`RestoreIncomplete`] with each site still owned, and the protection changes not undone.
pub(crate) fn restore(
    frozen: &Freeze<'_>,
    journal: &mut PatchJournal,
) -> Result<(), RestoreIncomplete> {
    let process = frozen.process();
    let mut failed = Vec::new();
    for i in (0..journal.patches.len()).rev() {
        if !journal.owned[i] {
            continue;
        }
        let patch = &journal.patches[i];
        let (site, at, original) = (patch.site, patch.at(), patch.original.clone());
        let mut current = vec![0; original.len()];
        let outcome = match process.read(at, &mut current) {
            Err(source) => Err(SiteFault::Memory(MemoryError::Read {
                at,
                len: original.len(),
                source,
            })),
            Ok(()) if !ours(&current, patch) => Err(SiteFault::Foreign),
            // Already original, perhaps after an attempt whose flush failed: flush before letting go.
            Ok(()) if current == original => process
                .flush(at, original.len() as u64)
                .map_err(|source| SiteFault::Memory(MemoryError::Flush { at, source })),
            Ok(()) => write_code(frozen, journal, at, &original)
                .and_then(|()| read_back(process, at, &original))
                .map_err(SiteFault::Memory),
        };
        match outcome {
            Ok(()) => journal.owned[i] = false,
            Err(fault) => failed.push(SiteFailure { site, fault }),
        }
    }
    let settled = settle(process, &mut journal.debt);
    failed.reverse();
    if failed.is_empty() && settled.is_ok() {
        Ok(())
    } else {
        Err(RestoreIncomplete {
            sites: failed,
            debt: journal.debt.len(),
        })
    }
}

/// Whether each byte of `current` is the patch's original or replacement byte at that offset.
fn ours(current: &[u8], patch: &Patch) -> bool {
    current
        .iter()
        .zip(patch.original.iter().zip(&patch.replacement))
        .all(|(&byte, (&original, &replacement))| byte == original || byte == replacement)
}

/// A code write or check that failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MemoryError {
    /// The memory could not be read.
    #[error("cannot read {len} bytes at {at}: {source}")]
    Read {
        /// Where.
        at: Va,
        /// How many bytes.
        len: usize,
        /// The failed call.
        source: OsError,
    },
    /// The region could not be queried.
    #[error("cannot query memory at {at}: {source}")]
    Query {
        /// Where.
        at: Va,
        /// The failed call.
        source: OsError,
    },
    /// The memory is not committed and accessible.
    #[error("{at} is not committed, accessible memory")]
    Inaccessible {
        /// Where.
        at: Va,
    },
    /// The memory could not be made writable.
    #[error("cannot make {at} writable: {source}")]
    Unprotect {
        /// Where.
        at: Va,
        /// The failed call.
        source: OsError,
    },
    /// The write failed; any part of it may have happened.
    #[error("cannot write {len} bytes at {at}: {source}")]
    Write {
        /// Where.
        at: Va,
        /// How many bytes.
        len: u64,
        /// The failed call.
        source: OsError,
    },
    /// The instruction cache could not be flushed.
    #[error("cannot flush the instruction cache at {at}: {source}")]
    Flush {
        /// Where.
        at: Va,
        /// The failed call.
        source: OsError,
    },
    /// A protection the trainer changed could not be put back.
    #[error("cannot restore the protection at {at}: {source}")]
    Reprotect {
        /// Where.
        at: Va,
        /// The failed call.
        source: OsError,
    },
    /// The memory does not read back as written.
    #[error("{at} does not read back as written")]
    Readback {
        /// Where.
        at: Va,
    },
}

/// Why patches could not be written.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PatchError {
    /// The patches do not fit together. A verified profile never does this.
    #[error("{site}: the patch does not fit its range or overlaps another")]
    Invalid {
        /// The site.
        site: SiteId,
    },
    /// The trainer already owns patches here.
    #[error("the hooks are already installed")]
    AlreadyOwned,
    /// A site no longer holds its original bytes.
    #[error("{site}: the game's code differs from the verified original")]
    Conflict {
        /// The site.
        site: SiteId,
    },
    /// A site's memory could not be read or written.
    #[error("{site}: {source}")]
    Memory {
        /// The site.
        site: SiteId,
        /// What failed.
        source: MemoryError,
    },
}

impl PatchError {
    /// The one next step to offer.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        match self {
            Self::Invalid { .. } | Self::AlreadyOwned => RecoveryAction::ReportBug,
            Self::Conflict { .. } => RecoveryAction::ModifiedGameCode,
            Self::Memory { .. } => RecoveryAction::Retry,
        }
    }
}

/// An installation that failed, and whether undoing it did.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{cause}")]
pub struct ApplyError {
    /// The first failure.
    pub cause: PatchError,
    /// The rollback of everything written before it.
    pub rollback: Result<(), RestoreIncomplete>,
}

/// Why one site could not be restored.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SiteFault {
    /// Its bytes are neither the original nor the trainer's; something else changed them.
    #[error("its bytes were changed by something else, so they were left alone")]
    Foreign,
    /// Its memory could not be read or written.
    #[error(transparent)]
    Memory(MemoryError),
}

/// A site still holding the trainer's code.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{site}: {fault}")]
pub struct SiteFailure {
    /// The site.
    pub site: SiteId,
    /// Why.
    pub fault: SiteFault,
}

/// A restoration that left some of the trainer's changes in the game.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("restoration incomplete: {} site(s) still hooked, {debt} protection change(s) not undone", sites.len())]
pub struct RestoreIncomplete {
    /// The sites still owned, in profile order.
    pub sites: Vec<SiteFailure>,
    /// Protection changes not undone.
    pub debt: usize,
}

impl RestoreIncomplete {
    /// The game's code is in an unknown state until it restarts.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        RecoveryAction::RestartGame
    }
}
