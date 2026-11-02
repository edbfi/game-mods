// SPDX-License-Identifier: AGPL-3.0-only
//! Where the cave allocation may go: every cave within a rel32 jump of its site.
//!
//! Cave `i` sits at `base + i * PAGE`, and the jump at site `i` reaches `base + i * PAGE` from `site + 5`
//! with a signed 32-bit displacement, both ends included. [`Reach`] intersects those bounds over every site.
//! [`candidates`] then takes the address space's free regions, as the platform reports them, and lists the
//! allocation bases inside both, aligned to the allocation granularity and nearest the middle of the reach
//! first. Trying them in order, and asking again after a lost race, is the caller's job.
#![forbid(unsafe_code)]

use crate::addr::{Rel32, Va};
use crate::profile::Profile;

use super::{PAGE, allocation_len};

/// The allocation bases from which every cave reaches its site, `lowest..=highest`, and the allocation's
/// length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reach {
    lowest: u64,
    highest: u64,
    len: u64,
}

impl Reach {
    /// The reach of `profile`'s caves for an image at `image`, or `None` if no base reaches every site, a
    /// site would end past the address space, or the profile has no site. The lowest base is never below a
    /// page: address 0 asks an allocator for any address at all.
    #[must_use]
    pub fn new(profile: &Profile, image: Va) -> Option<Self> {
        let len = allocation_len(profile);
        let mut lowest = i128::from(PAGE);
        let mut highest = i128::from(u64::MAX) - i128::from(len);
        if profile.sites.is_empty() || highest < 0 {
            return None;
        }
        for (page, site) in profile.sites.iter().enumerate() {
            let at = image.at(site.rva).ok()?;
            let site_len = site.original.len().max(Rel32::JMP_LEN as usize) as u64;
            at.checked_add(site_len).ok()?;
            let next = i128::from(at.get()) + i128::from(Rel32::JMP_LEN);
            let cave = page as i128 * i128::from(PAGE);
            lowest = lowest.max(next + i128::from(i32::MIN) - cave);
            highest = highest.min(next + i128::from(i32::MAX) - cave);
        }
        (lowest <= highest).then_some(Self {
            // Both lie in PAGE..=u64::MAX - len after the clamps above.
            lowest: lowest as u64,
            highest: highest as u64,
            len,
        })
    }

    /// The lowest base.
    #[must_use]
    pub const fn lowest(self) -> Va {
        Va::new(self.lowest)
    }

    /// The highest base.
    #[must_use]
    pub const fn highest(self) -> Va {
        Va::new(self.highest)
    }

    /// The allocation's size: one page per site.
    #[must_use]
    pub const fn size(self) -> u64 {
        self.len
    }

    /// The middle of the reach, which candidates are ordered around.
    #[must_use]
    pub const fn middle(self) -> Va {
        Va::new(self.lowest + (self.highest - self.lowest) / 2)
    }

    /// Whether the allocation can start at `base`.
    #[must_use]
    pub const fn contains(self, base: Va) -> bool {
        self.lowest <= base.get() && base.get() <= self.highest
    }
}

/// A free region of the game's address space, `[start, start + len)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FreeRegion {
    /// The first free byte.
    pub start: Va,
    /// The number of free bytes.
    pub len: u64,
}

/// One allocation base per free region that holds the whole allocation inside `reach`, aligned to
/// `granularity` and as near the reach's middle as the region allows, nearest first.
///
/// `granularity` must be a power of two of at least a page; for any other value the list is empty.
#[must_use]
pub fn candidates(
    reach: Reach,
    granularity: u64,
    free: impl IntoIterator<Item = FreeRegion>,
) -> Vec<Va> {
    if !granularity.is_power_of_two() || granularity < u64::from(PAGE) {
        return Vec::new();
    }
    let middle = reach.middle().get();
    let mut found: Vec<u64> = free
        .into_iter()
        .filter_map(|region| {
            let end = region.start.get().checked_add(region.len)?;
            let first = align_up(region.start.get().max(reach.lowest), granularity)?;
            let last = align_down(end.checked_sub(reach.len)?.min(reach.highest), granularity);
            if first > last {
                return None;
            }
            let near = align_down(middle, granularity).clamp(first, last);
            let above = near.checked_add(granularity).filter(|&up| up <= last);
            Some(match above {
                Some(up) if up.abs_diff(middle) < near.abs_diff(middle) => up,
                _ => near,
            })
        })
        .collect();
    found.sort_unstable_by_key(|base| (base.abs_diff(middle), *base));
    found.dedup();
    found.into_iter().map(Va::new).collect()
}

/// `value` rounded up to a multiple of `align`, a power of two, or `None` past the address space.
fn align_up(value: u64, align: u64) -> Option<u64> {
    value
        .checked_add(align - 1)
        .map(|value| value & !(align - 1))
}

/// `value` rounded down to a multiple of `align`, a power of two.
const fn align_down(value: u64, align: u64) -> u64 {
    value & !(align - 1)
}
