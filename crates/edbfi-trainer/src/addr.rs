// SPDX-License-Identifier: AGPL-3.0-or-later
//! Image-relative and absolute addresses, and the rel32 displacement of an `E9` jump.
#![forbid(unsafe_code)]

use std::fmt;
use std::num::NonZeroU32;

use crate::recovery::RecoveryAction;

/// An offset from the start of a loaded image (a relative virtual address).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rva(u32);

impl Rva {
    /// Wraps a raw relative virtual address.
    #[must_use]
    pub const fn new(rva: u32) -> Self {
        Self(rva)
    }

    /// The raw offset.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// `self + n`, or `None` on overflow.
    #[must_use]
    pub const fn checked_add(self, n: u32) -> Option<Self> {
        match self.0.checked_add(n) {
            Some(rva) => Some(Self(rva)),
            None => None,
        }
    }
}

impl fmt::Display for Rva {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

/// An absolute virtual address in a 64-bit process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Va(u64);

impl Va {
    /// Wraps a raw virtual address.
    #[must_use]
    pub const fn new(va: u64) -> Self {
        Self(va)
    }

    /// The raw address.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The address `rva` bytes past this image base.
    ///
    /// # Errors
    ///
    /// [`AddressError::Overflow`] if the sum leaves the 64-bit address space.
    pub fn at(self, rva: Rva) -> Result<Self, AddressError> {
        self.checked_add(u64::from(rva.get()))
    }

    /// `self + n`.
    ///
    /// # Errors
    ///
    /// [`AddressError::Overflow`] if the sum leaves the 64-bit address space.
    pub fn checked_add(self, n: u64) -> Result<Self, AddressError> {
        self.0
            .checked_add(n)
            .map(Self)
            .ok_or(AddressError::Overflow {
                base: self,
                offset: n,
            })
    }
}

impl fmt::Display for Va {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

/// A non-empty half-open range of code, `[start, start + len)`, that does not wrap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CodeRange {
    start: Va,
    len: NonZeroU32,
}

impl CodeRange {
    /// The range of `len` bytes from `start`.
    ///
    /// # Errors
    ///
    /// [`AddressError::Overflow`] if the range would pass the end of the address space.
    pub fn new(start: Va, len: NonZeroU32) -> Result<Self, AddressError> {
        start.checked_add(u64::from(len.get()))?;
        Ok(Self { start, len })
    }

    /// The first byte.
    #[must_use]
    pub const fn start(self) -> Va {
        self.start
    }

    /// The number of bytes.
    #[must_use]
    pub const fn len(self) -> NonZeroU32 {
        self.len
    }

    /// One past the last byte. Cannot overflow: [`CodeRange::new`] checked it.
    #[must_use]
    pub const fn end(self) -> u64 {
        self.start.0 + self.len.get() as u64
    }

    /// Whether `va` lies inside the range.
    #[must_use]
    pub const fn contains(self, va: Va) -> bool {
        self.start.0 <= va.0 && va.0 < self.end()
    }

    /// Whether the two ranges share at least one byte.
    #[must_use]
    pub const fn overlaps(self, other: Self) -> bool {
        self.start.0 < other.end() && other.start.0 < self.end()
    }
}

/// The signed displacement of a 5-byte `E9 rel32` jump, measured from the end of the jump.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rel32(i32);

impl Rel32 {
    /// Length of the `E9 rel32` instruction.
    pub const JMP_LEN: u64 = 5;

    /// The displacement that makes a jump at `site` land on `target`.
    ///
    /// # Errors
    ///
    /// [`AddressError::Overflow`] if `site + 5` overflows, or [`AddressError::OutOfRel32`] if `target` is not
    /// reachable with a signed 32-bit displacement. Both endpoints, `i32::MIN` and `i32::MAX`, are reachable.
    pub fn from_site(site: Va, target: Va) -> Result<Self, AddressError> {
        let next = site.checked_add(Self::JMP_LEN)?;
        let delta = i128::from(target.0) - i128::from(next.0);
        i32::try_from(delta)
            .map(Self)
            .map_err(|_| AddressError::OutOfRel32 { site, target })
    }

    /// The raw displacement.
    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }

    /// The complete jump instruction.
    #[must_use]
    pub fn jmp_bytes(self) -> [u8; 5] {
        let [a, b, c, d] = self.0.to_le_bytes();
        [0xE9, a, b, c, d]
    }
}

/// An address computation that does not fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AddressError {
    /// The sum left the 64-bit address space.
    #[error("address {base} + {offset:#x} overflows")]
    Overflow {
        /// The starting address.
        base: Va,
        /// The offset added to it.
        offset: u64,
    },
    /// A jump target lies more than 2 GiB from its site.
    #[error("jump target {target} is out of rel32 range of site {site}")]
    OutOfRel32 {
        /// Where the jump is written.
        site: Va,
        /// Where it should land.
        target: Va,
    },
}

impl AddressError {
    /// Addresses come from a verified image and the trainer's own allocation, so either failure is a bug.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        RecoveryAction::ReportBug
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn len(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).unwrap()
    }

    #[test]
    fn rel32_reaches_both_inclusive_endpoints() {
        let site = Va::new(0x1_0000_0000);
        let next = 0x1_0000_0005_i128;
        for delta in [i128::from(i32::MIN), i128::from(i32::MAX)] {
            let target = Va::new(u64::try_from(next + delta).unwrap());
            assert_eq!(
                i128::from(Rel32::from_site(site, target).unwrap().get()),
                delta
            );
        }
        for delta in [i128::from(i32::MIN) - 1, i128::from(i32::MAX) + 1] {
            let target = Va::new(u64::try_from(next + delta).unwrap());
            assert_eq!(
                Rel32::from_site(site, target),
                Err(AddressError::OutOfRel32 { site, target })
            );
        }
    }

    #[test]
    fn rel32_site_end_overflow_is_an_error() {
        let site = Va::new(u64::MAX - 4);
        assert!(matches!(
            Rel32::from_site(site, Va::new(0)),
            Err(AddressError::Overflow { .. })
        ));
        assert!(Rel32::from_site(Va::new(u64::MAX - 5), Va::new(u64::MAX)).is_ok());
    }

    #[test]
    fn jmp_bytes_are_little_endian() {
        let rel = Rel32::from_site(Va::new(0x1000), Va::new(0x0FF0)).unwrap();
        assert_eq!(rel.get(), -0x15);
        assert_eq!(rel.jmp_bytes(), [0xE9, 0xEB, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn va_at_checks_overflow() {
        assert_eq!(
            Va::new(0x1_4000_0000).at(Rva::new(0x1000)),
            Ok(Va::new(0x1_4000_1000))
        );
        assert!(Va::new(u64::MAX).at(Rva::new(1)).is_err());
    }

    #[test]
    fn code_range_rejects_wrap_and_tests_bounds() {
        assert!(CodeRange::new(Va::new(u64::MAX), len(1)).is_err());
        let last = CodeRange::new(Va::new(u64::MAX - 1), len(1)).unwrap();
        assert_eq!(last.end(), u64::MAX);
        let range = CodeRange::new(Va::new(0x100), len(5)).unwrap();
        assert!(range.contains(Va::new(0x100)) && range.contains(Va::new(0x104)));
        assert!(!range.contains(Va::new(0xFF)) && !range.contains(Va::new(0x105)));
        let touching = CodeRange::new(Va::new(0x105), len(1)).unwrap();
        assert!(!range.overlaps(touching) && !touching.overlaps(range));
        let inside = CodeRange::new(Va::new(0x104), len(9)).unwrap();
        assert!(range.overlaps(inside) && inside.overlaps(range));
    }

    proptest! {
        #[test]
        fn rel32_lands_on_its_target(site in 0u64..u64::MAX - 5, delta in any::<i32>()) {
            let target = i128::from(site) + 5 + i128::from(delta);
            prop_assume!((0..=i128::from(u64::MAX)).contains(&target));
            let target = Va::new(u64::try_from(target).unwrap());
            let rel = Rel32::from_site(Va::new(site), target).unwrap();
            prop_assert_eq!(rel.get(), delta);
        }
    }
}
