// SPDX-License-Identifier: AGPL-3.0-or-later
//! The 32-bit control word shared with the game, and the masks that feed it.
//!
//! Hooks read one aligned 32-bit word to decide whether a cheat applies. The trainer computes a
//! [`DesiredMask`] from the user's choices, publishes it, and only then reports an [`Ack`] carrying the
//! [`ControlMask`] the game can now see. The dashboard shows acknowledged state, never the wish.
#![forbid(unsafe_code)]

use std::fmt;

/// One bit of the control word, `0..32`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ControlBit(u8);

impl ControlBit {
    /// Bit `index`.
    ///
    /// # Panics
    ///
    /// If `index >= 32`; in a constant this is a build error:
    ///
    /// ```compile_fail
    /// const BAD: edbfi_trainer::control::ControlBit = edbfi_trainer::control::ControlBit::new(32);
    /// ```
    #[must_use]
    pub const fn new(index: u8) -> Self {
        assert!(index < 32, "control bits are 0..32");
        Self(index)
    }

    /// Bit `index`, or `None` if it is out of range.
    #[must_use]
    pub const fn try_new(index: u8) -> Option<Self> {
        if index < 32 { Some(Self(index)) } else { None }
    }

    /// The bit's position.
    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }

    /// The word with only this bit set.
    #[must_use]
    pub const fn word(self) -> u32 {
        1 << self.0
    }
}

/// A set of control bits, such as the bits one cheat drives.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ControlSet(u32);

impl ControlSet {
    /// No bits.
    pub const EMPTY: Self = Self(0);

    /// Exactly `bit`.
    #[must_use]
    pub const fn of(bit: ControlBit) -> Self {
        Self(bit.word())
    }

    /// The bits `first..=last`.
    ///
    /// # Panics
    ///
    /// If `first > last`.
    #[must_use]
    pub const fn span(first: ControlBit, last: ControlBit) -> Self {
        assert!(first.0 <= last.0, "an empty control bit span");
        let high = if last.0 == 31 {
            u32::MAX
        } else {
            (1 << (last.0 + 1)) - 1
        };
        Self(high & !(first.word() - 1))
    }

    /// This set plus `bit`.
    #[must_use]
    pub const fn with(self, bit: ControlBit) -> Self {
        Self(self.0 | bit.word())
    }

    /// Bits in either set.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether `bit` is in the set.
    #[must_use]
    pub const fn contains(self, bit: ControlBit) -> bool {
        self.0 & bit.word() != 0
    }

    /// Whether the sets share a bit.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Whether the set has no bits.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The raw word.
    #[must_use]
    pub const fn word(self) -> u32 {
        self.0
    }

    /// The bits in ascending order.
    pub fn iter(self) -> impl Iterator<Item = ControlBit> {
        (0..32)
            .filter(move |&index| self.0 & (1 << index) != 0)
            .map(ControlBit)
    }
}

impl fmt::Debug for ControlSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ControlSet({:#010x})", self.0)
    }
}

/// The word the trainer wants the game to see: the effective result of selections, options and master.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct DesiredMask(ControlSet);

impl DesiredMask {
    /// Every cheat off. Published before installation and before restoration.
    pub const ZERO: Self = Self(ControlSet::EMPTY);

    /// The mask enabling exactly `bits`.
    #[must_use]
    pub const fn new(bits: ControlSet) -> Self {
        Self(bits)
    }

    /// The bits it enables.
    #[must_use]
    pub const fn bits(self) -> ControlSet {
        self.0
    }
}

impl fmt::Debug for DesiredMask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DesiredMask({:#010x})", self.0.word())
    }
}

/// A word that has been published to the game.
///
/// Only the publisher creates one, so holding a `ControlMask` proves the game can observe it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ControlMask(ControlSet);

impl ControlMask {
    pub(crate) const fn published(desired: DesiredMask) -> Self {
        Self(desired.0)
    }

    /// The bits the game sees.
    #[must_use]
    pub const fn bits(self) -> ControlSet {
        self.0
    }
}

impl fmt::Debug for ControlMask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ControlMask({:#010x})", self.0.word())
    }
}

/// Confirmation that a publication completed.
///
/// Revisions increase with every publication of a session, so a stale acknowledgement can be told apart from
/// the current one even when both carry the same mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ack {
    revision: u64,
    mask: ControlMask,
}

impl Ack {
    pub(crate) const fn new(revision: u64, mask: ControlMask) -> Self {
        Self { revision, mask }
    }

    /// An acknowledgement of `desired` at `revision` that no game saw, for simulations and dashboard tests.
    #[cfg(feature = "simulate")]
    #[must_use]
    pub const fn simulated(revision: u64, desired: DesiredMask) -> Self {
        Self::new(revision, ControlMask::published(desired))
    }

    /// The publication's sequence number.
    #[must_use]
    pub const fn revision(self) -> u64 {
        self.revision
    }

    /// The word the game now sees.
    #[must_use]
    pub const fn mask(self) -> ControlMask {
        self.mask
    }
}

/// The trainer-wide switch. Paused publishes zero while keeping every selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Master {
    /// Selected cheats apply.
    #[default]
    Active,
    /// Nothing applies; selections are kept.
    Paused,
}

impl Master {
    /// The other state.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Active => Self::Paused,
            Self::Paused => Self::Active,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn bit_range_is_checked() {
        assert_eq!(
            ControlBit::try_new(31).map(ControlBit::word),
            Some(0x8000_0000)
        );
        assert_eq!(ControlBit::try_new(32), None);
        assert_eq!(ControlBit::new(0).word(), 1);
    }

    #[test]
    #[should_panic(expected = "control bits are 0..32")]
    fn bit_constructor_panics_out_of_range() {
        let _ = ControlBit::new(32);
    }

    #[test]
    fn spans_cover_the_documented_layout() {
        let classes = ControlSet::span(ControlBit::new(7), ControlBit::new(30));
        assert_eq!(classes.word(), 0x7FFF_FF80);
        assert_eq!(classes.iter().count(), 24);
        assert_eq!(
            ControlSet::span(ControlBit::new(0), ControlBit::new(31)).word(),
            u32::MAX
        );
        assert_eq!(
            ControlSet::span(ControlBit::new(31), ControlBit::new(31)).word(),
            0x8000_0000
        );
        assert!(!classes.contains(ControlBit::new(31)) && classes.contains(ControlBit::new(7)));
    }

    #[test]
    fn ack_carries_the_published_mask() {
        let desired = DesiredMask::new(ControlSet::of(ControlBit::new(3)));
        let ack = Ack::new(7, ControlMask::published(desired));
        assert_eq!((ack.revision(), ack.mask().bits()), (7, desired.bits()));
        assert_eq!(Master::Active.toggled().toggled(), Master::Active);
        assert_eq!(Master::default(), Master::Active);
    }

    proptest! {
        #[test]
        fn iter_rebuilds_the_set(word in any::<u32>()) {
            let set = ControlSet(word);
            let rebuilt = set.iter().fold(ControlSet::EMPTY, ControlSet::with);
            prop_assert_eq!(rebuilt, set);
            prop_assert!(set.iter().all(|bit| set.contains(bit)));
        }

        #[test]
        fn span_matches_a_loop(first in 0u8..32, len in 0u8..32) {
            let last = first.saturating_add(len).min(31);
            let expected = (first..=last).fold(0u32, |word, bit| word | 1 << bit);
            prop_assert_eq!(ControlSet::span(ControlBit::new(first), ControlBit::new(last)).word(), expected);
        }
    }
}
