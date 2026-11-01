// SPDX-License-Identifier: AGPL-3.0-only
//! Profile-bound ids, the user's selection, and the effective control mask.
//!
//! Ids, selections and option sets are minted only by a [`ValidProfile`] and carry its brand, so an id from one
//! profile cannot silently index another: mixing them is a bug and panics.
#![forbid(unsafe_code)]

use std::fmt;

use crate::control::{ControlSet, DesiredMask, Master};

use super::{Class, GameOption, Profile, ProfileError, Toggle};

/// A fingerprint of a profile's keys and bits, carried by every value that indexes it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Brand(u64);

impl Brand {
    /// FNV-1a over the slug, every toggle key and its bits, and every option key.
    fn of(profile: &Profile) -> Self {
        const PRIME: u64 = 0x0000_0100_0000_01B3;
        let mut hash = 0xCBF2_9CE4_8422_2325_u64;
        let mut eat = |bytes: &[u8]| {
            for &byte in bytes.iter().chain(&[0xFF]) {
                hash = (hash ^ u64::from(byte)).wrapping_mul(PRIME);
            }
        };
        eat(profile.slug.game.as_bytes());
        eat(profile.slug.r#mod.as_bytes());
        for toggle in profile.toggles {
            eat(toggle.key.as_bytes());
            eat(&toggle.controls.word().to_le_bytes());
        }
        for option in profile.options {
            eat(option.key.as_bytes());
        }
        Self(hash)
    }

    fn assert(self, other: Self) {
        assert!(self == other, "an id or selection from another profile");
    }
}

impl fmt::Debug for Brand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// A toggle of one [`ValidProfile`]: its position in [`Profile::toggles`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ToggleId {
    brand: Brand,
    index: u8,
}

impl ToggleId {
    /// The toggle's position in its profile's display order.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index as usize
    }
}

/// An option of one [`ValidProfile`]: its position in [`Profile::options`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OptionId {
    brand: Brand,
    index: u8,
}

impl OptionId {
    /// The option's position in its profile's display order.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index as usize
    }
}

/// A weapon class of one [`ValidProfile`]: its position in [`Profile::classes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClassId {
    brand: Brand,
    index: u8,
}

impl ClassId {
    /// The class's position in its profile's class order.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index as usize
    }
}

/// The selected toggles of one profile. Selections are kept while the master is paused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Selection {
    brand: Brand,
    bits: u64,
}

impl Selection {
    /// Whether `id` is selected.
    ///
    /// # Panics
    ///
    /// If `id` belongs to another profile.
    #[must_use]
    pub fn contains(self, id: ToggleId) -> bool {
        self.brand.assert(id.brand);
        self.bits & 1 << id.index != 0
    }

    /// Selects or clears `id`.
    ///
    /// # Panics
    ///
    /// If `id` belongs to another profile.
    pub fn set(&mut self, id: ToggleId, on: bool) {
        self.brand.assert(id.brand);
        if on {
            self.bits |= 1 << id.index;
        } else {
            self.bits &= !(1 << id.index);
        }
    }

    /// Whether nothing is selected.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }
}

/// The enabled options of one profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OptionSet {
    brand: Brand,
    bits: u16,
}

impl OptionSet {
    /// Whether `id` is on.
    ///
    /// # Panics
    ///
    /// If `id` belongs to another profile.
    #[must_use]
    pub fn contains(self, id: OptionId) -> bool {
        self.brand.assert(id.brand);
        self.bits & 1 << id.index != 0
    }

    /// Turns `id` on or off.
    ///
    /// # Panics
    ///
    /// If `id` belongs to another profile.
    pub fn set(&mut self, id: OptionId, on: bool) {
        self.brand.assert(id.brand);
        if on {
            self.bits |= 1 << id.index;
        } else {
            self.bits &= !(1 << id.index);
        }
    }
}

/// What the user asked for: the master switch, the selection and the options.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Desired {
    /// The master switch.
    pub master: Master,
    /// The selected toggles.
    pub selection: Selection,
    /// The enabled options.
    pub options: OptionSet,
}

/// A derived bit with its option and triggers resolved to positions.
#[derive(Clone, Copy, Debug)]
struct Rule {
    bits: ControlSet,
    option: u16,
    triggers: u64,
}

/// A profile that passed [`Profile::validate`]: the only source of its ids, selections and option sets.
#[derive(Clone, Debug)]
pub struct ValidProfile<'p> {
    profile: &'p Profile,
    brand: Brand,
    rules: Box<[Rule]>,
    class_toggles: Box<[u8]>,
}

impl<'p> ValidProfile<'p> {
    /// Validates `profile`. A profile that ran `const _: () = PROFILE.check();` always passes.
    ///
    /// # Errors
    ///
    /// The [`ProfileError`] from [`Profile::validate`].
    pub fn new(profile: &'p Profile) -> Result<Self, ProfileError> {
        profile.validate()?;
        let index = |position: Option<usize>| {
            position
                .and_then(|at| u8::try_from(at).ok())
                .ok_or(ProfileError::TooMany)
        };
        let rules = profile
            .derived
            .iter()
            .map(|rule| {
                let option = index(profile.option_index(rule.option))?;
                let triggers = rule.when_any.iter().try_fold(0u64, |bits, key| {
                    Ok::<_, ProfileError>(bits | 1 << index(profile.toggle_index(key))?)
                })?;
                Ok(Rule {
                    bits: ControlSet::of(rule.bit),
                    option: 1 << option,
                    triggers,
                })
            })
            .collect::<Result<_, ProfileError>>()?;
        let class_toggles = profile
            .classes
            .iter()
            .map(|class| index(profile.toggle_index(class.key)))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            profile,
            brand: Brand::of(profile),
            rules,
            class_toggles,
        })
    }

    /// The profile's data.
    #[must_use]
    pub const fn profile(&self) -> &'p Profile {
        self.profile
    }

    /// Every toggle with its id, in display order.
    pub fn toggles(&self) -> impl ExactSizeIterator<Item = (ToggleId, &'p Toggle)> + use<'p> {
        let brand = self.brand;
        self.profile
            .toggles
            .iter()
            .enumerate()
            .map(move |(index, toggle)| {
                // Validation bounds every table well below 256 entries.
                let index = index as u8;
                (ToggleId { brand, index }, toggle)
            })
    }

    /// The toggle with settings key `key`.
    #[must_use]
    pub fn toggle_id(&self, key: &str) -> Option<ToggleId> {
        self.toggles()
            .find(|(_, toggle)| toggle.key == key)
            .map(|(id, _)| id)
    }

    /// The toggle `id` names.
    ///
    /// # Panics
    ///
    /// If `id` belongs to another profile.
    #[must_use]
    pub fn toggle(&self, id: ToggleId) -> &'p Toggle {
        self.brand.assert(id.brand);
        &self.profile.toggles[id.index()]
    }

    /// Every option with its id, in display order.
    pub fn options(&self) -> impl ExactSizeIterator<Item = (OptionId, &'p GameOption)> + use<'p> {
        let brand = self.brand;
        self.profile
            .options
            .iter()
            .enumerate()
            .map(move |(index, option)| {
                // Validation bounds every table well below 256 entries.
                let index = index as u8;
                (OptionId { brand, index }, option)
            })
    }

    /// The option with settings key `key`.
    #[must_use]
    pub fn option_id(&self, key: &str) -> Option<OptionId> {
        self.options()
            .find(|(_, option)| option.key == key)
            .map(|(id, _)| id)
    }

    /// The option `id` names.
    ///
    /// # Panics
    ///
    /// If `id` belongs to another profile.
    #[must_use]
    pub fn option(&self, id: OptionId) -> &'p GameOption {
        self.brand.assert(id.brand);
        &self.profile.options[id.index()]
    }

    /// Every weapon class with its id, in class order.
    pub fn classes(&self) -> impl ExactSizeIterator<Item = (ClassId, &'p Class)> + use<'p> {
        let brand = self.brand;
        self.profile
            .classes
            .iter()
            .enumerate()
            .map(move |(index, class)| {
                // Validation bounds every table well below 256 entries.
                let index = index as u8;
                (ClassId { brand, index }, class)
            })
    }

    /// The toggle selecting class `id`.
    ///
    /// # Panics
    ///
    /// If `id` belongs to another profile.
    #[must_use]
    pub fn class_toggle(&self, id: ClassId) -> ToggleId {
        self.brand.assert(id.brand);
        ToggleId {
            brand: self.brand,
            index: self.class_toggles[id.index()],
        }
    }

    /// A selection with nothing selected.
    #[must_use]
    pub const fn no_selection(&self) -> Selection {
        Selection {
            brand: self.brand,
            bits: 0,
        }
    }

    /// An option set with every option off.
    #[must_use]
    pub const fn no_options(&self) -> OptionSet {
        OptionSet {
            brand: self.brand,
            bits: 0,
        }
    }

    /// First-run state: master active, each toggle and option at its default.
    #[must_use]
    pub fn defaults(&self) -> Desired {
        let mut selection = self.no_selection();
        for (id, toggle) in self.toggles() {
            selection.set(id, toggle.default);
        }
        let mut options = self.no_options();
        for (id, option) in self.options() {
            options.set(id, option.default);
        }
        Desired {
            master: Master::Active,
            selection,
            options,
        }
    }

    /// The mask to publish for `desired`.
    ///
    /// Paused publishes zero. Otherwise each selected toggle adds its bits, so a class selected under a
    /// selected all-classes toggle changes nothing, and each derived bit is set when its option is on and any
    /// of its triggers is selected.
    ///
    /// # Panics
    ///
    /// If `desired` belongs to another profile.
    #[must_use]
    pub fn effective(&self, desired: &Desired) -> DesiredMask {
        self.brand.assert(desired.selection.brand);
        self.brand.assert(desired.options.brand);
        if desired.master == Master::Paused {
            return DesiredMask::ZERO;
        }
        let selected = desired.selection.bits;
        let mut bits = self
            .profile
            .toggles
            .iter()
            .enumerate()
            .filter(|(index, _)| selected & 1 << index != 0)
            .fold(ControlSet::EMPTY, |bits, (_, toggle)| {
                bits.union(toggle.controls)
            });
        for rule in &self.rules {
            if desired.options.bits & rule.option != 0 && selected & rule.triggers != 0 {
                bits = bits.union(rule.bits);
            }
        }
        DesiredMask::new(bits)
    }
}
