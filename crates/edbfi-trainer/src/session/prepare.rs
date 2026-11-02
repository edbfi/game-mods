// SPDX-License-Identifier: AGPL-3.0-only
//! Putting the zeroed control word and the proven caves into the game, before any site is hooked.
//!
//! The cave block goes where every site can reach its cave with a rel32 jump. The free regions in that reach
//! come from walking the address space; the candidate bases are tried nearest the middle first. Another
//! allocation can take a region between the walk and the attempt, so a failed attempt moves on to the next
//! candidate, and a few fresh walks follow before giving up. Each cave is written while the block is still
//! read-write, read back, and only then is the block made execute-read and the instruction cache flushed.
#![forbid(unsafe_code)]

use crate::addr::Va;
use crate::cave::place::{FreeRegion, Reach, candidates};
use crate::cave::{self, HookError};
use crate::patch::{OsError, Patch, PatchError, PatchJournal, Protection, RegionState};
use crate::profile::ValidProfile;
use crate::recovery::RecoveryAction;

use super::{Allocation, Game, Hooks};

/// `ERROR_INVALID_ADDRESS`: the requested base is no longer free.
pub(crate) const ERROR_INVALID_ADDRESS: u32 = 487;

/// `ERROR_PROC_NOT_FOUND`: a function this Windows does not export.
const ERROR_PROC_NOT_FOUND: u32 = 127;

/// Fresh walks of the address space before placement gives up.
const PLACEMENT_ROUNDS: usize = 3;
/// Most regions one walk visits.
const MAX_REGIONS: usize = 1 << 16;

/// Maps the controls, places and writes the caves, and returns everything needed to hook the sites.
pub(super) fn prepare(
    game: &dyn Game,
    profile: &ValidProfile<'static>,
    image: Va,
) -> Result<Hooks, PrepareError> {
    let controls = game.controls().map_err(PrepareError::Controls)?;
    let reach = Reach::new(profile.profile(), image).ok_or(PrepareError::NoReach)?;
    let block = place(game, reach)?;
    let plan = cave::plan(profile, image, block.base(), controls.remote())?;
    for hook in plan.hooks() {
        let cave = hook.cave();
        game.write(cave.address(), cave.bytes())
            .map_err(|source| PrepareError::Write {
                at: cave.address(),
                source,
            })?;
        let mut back = vec![0; cave.bytes().len()];
        game.read(cave.address(), &mut back)
            .map_err(|source| PrepareError::Read {
                at: cave.address(),
                source,
            })?;
        if back != cave.bytes() {
            return Err(PrepareError::Readback { at: cave.address() });
        }
    }
    let len = reach.size();
    game.protect(block.base(), len, Protection::EXECUTE_READ)
        .map_err(PrepareError::Protect)?;
    game.flush(block.base(), len)
        .map_err(PrepareError::Protect)?;
    let journal = PatchJournal::new(
        plan.hooks()
            .iter()
            .map(|hook| Patch {
                site: hook.site().id,
                range: hook.range(),
                original: hook.original().to_vec(),
                replacement: hook.jump().to_vec(),
            })
            .collect(),
    )
    .map_err(PrepareError::Patches)?;
    Ok(Hooks {
        controls,
        plan,
        journal,
        unpublished: Some(block),
        stuck: crate::patch::Stuck::default(),
    })
}

/// Allocates the cave block at the best free base within `reach`.
fn place(game: &dyn Game, reach: Reach) -> Result<Box<dyn Allocation>, PrepareError> {
    let granularity = game.granularity();
    for _ in 0..PLACEMENT_ROUNDS {
        let free = free_regions(game, reach)?;
        for base in candidates(reach, granularity, free) {
            match game.allocate(base, reach.size()) {
                Ok(block) => return Ok(block),
                // Taken since the walk: try the next base.
                Err(error) if error.code == ERROR_INVALID_ADDRESS => {}
                Err(error) => return Err(PrepareError::Allocate(error)),
            }
        }
    }
    Err(PrepareError::NoSpace)
}

/// The free regions overlapping the allocation bases `reach` allows, from one walk of the address space.
pub(super) fn free_regions(game: &dyn Game, reach: Reach) -> Result<Vec<FreeRegion>, PrepareError> {
    let end = reach.highest().get().saturating_add(reach.size());
    let mut cursor = reach.lowest().get();
    let mut free = Vec::new();
    for _ in 0..MAX_REGIONS {
        if cursor >= end {
            break;
        }
        let region = game.query(Va::new(cursor)).map_err(PrepareError::Query)?;
        let Some(region_end) = region.end().filter(|&e| e > cursor) else {
            break;
        };
        if region.state == RegionState::Free {
            free.push(FreeRegion {
                start: region.base,
                len: region.size,
            });
        }
        cursor = region_end;
    }
    Ok(free)
}

/// Why the caves or controls could not be put in the game.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PrepareError {
    /// The shared control word could not be mapped into the game.
    #[error("cannot share the control word with the game: {0}")]
    Controls(OsError),
    /// No base reaches every site. A verified profile never does this.
    #[error("no cave address can reach every hook site")]
    NoReach,
    /// The address space was walked but not read.
    #[error("cannot read the game's address space: {0}")]
    Query(OsError),
    /// No free space within reach of every site could be allocated.
    #[error("no free memory within reach of every hook site")]
    NoSpace,
    /// A cave could not be built. A verified profile never does this.
    #[error(transparent)]
    Hook(#[from] HookError),
    /// A cave could not be written.
    #[error("cannot write the cave at {at}: {source}")]
    Write {
        /// Where.
        at: Va,
        /// The failed call.
        source: OsError,
    },
    /// A cave could not be read back.
    #[error("cannot read back the cave at {at}: {source}")]
    Read {
        /// Where.
        at: Va,
        /// The failed call.
        source: OsError,
    },
    /// Memory near the game could not be allocated, for a reason other than a lost race.
    #[error("cannot allocate the caves: {0}")]
    Allocate(OsError),
    /// A cave does not read back as written.
    #[error("the cave at {at} does not read back as written")]
    Readback {
        /// Where.
        at: Va,
    },
    /// The caves could not be made executable.
    #[error("cannot make the caves executable: {0}")]
    Protect(OsError),
    /// The patches do not fit together. A verified profile never does this.
    #[error(transparent)]
    Patches(PatchError),
}

impl PrepareError {
    /// The one next step to offer.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        match self {
            // Only a missing `MapViewOfFileNuma2` means Windows is too old.
            Self::Controls(error) if error.code == ERROR_PROC_NOT_FOUND => {
                RecoveryAction::UpdateWindows
            }
            Self::Controls(_) | Self::Read { .. } | Self::Allocate(_) => RecoveryAction::Retry,
            Self::NoReach | Self::Hook(_) | Self::Patches(_) => RecoveryAction::ReportBug,
            Self::Query(_)
            | Self::NoSpace
            | Self::Write { .. }
            | Self::Readback { .. }
            | Self::Protect(_) => RecoveryAction::Retry,
        }
    }
}
