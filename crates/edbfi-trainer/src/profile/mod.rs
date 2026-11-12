// SPDX-License-Identifier: AGPL-3.0-or-later
//! A game profile: everything the trainer knows about one supported game build, written as reviewed data.
//!
//! A profile declares the build's identity, the player and weapon layout, the cheats a user can select, the
//! derived bits computed from options, the weapon classes, and every hook site with its original bytes and
//! context signature. Every static field is public so a profile is a plain `const`; a mod ends its profile with
//! `const _: () = PROFILE.check();` so a broken invariant fails the build. At run time [`ValidProfile`] wraps
//! a checked profile and is the only source of the [`ToggleId`], [`OptionId`] and [`ClassId`] values that
//! index it.
//!
//! [`HookKind`] is a closed vocabulary of reviewed hook shapes, not a patching language: extend it only when a
//! reviewed new site needs a shape it cannot express.
#![forbid(unsafe_code)]

mod check;
#[cfg(any(test, feature = "simulate"))]
pub mod dummy;
mod select;
pub mod synthetic;
#[cfg(test)]
mod tests;

pub use check::ProfileError;
pub use select::{
    ClassId, Desired, OptionId, OptionSet, Selection, ToggleId, ToggleMap, ValidProfile,
};

use std::fmt;

use crate::addr::Rva;
use crate::aob::Pattern;
use crate::chord::Chord;
use crate::control::{ControlBit, ControlSet};
use crate::identity::Sha256Digest;

/// One supported build of one game, as data.
#[derive(Clone, Copy, Debug)]
pub struct Profile {
    /// The game's names.
    pub game: Game,
    /// The repository folders of the mod, which also name its settings folder.
    pub slug: ModSlug,
    /// How the supported build is recognised.
    pub identity: Identity,
    /// Where hooks find the player and a weapon's state.
    pub player: PlayerLayout,
    /// Dashboard sections, in display order.
    pub groups: &'static [Group],
    /// What a user can select, in display order. A toggle's position is its [`ToggleId`].
    pub toggles: &'static [Toggle],
    /// Game settings that are not cheats, in display order.
    pub options: &'static [GameOption],
    /// Bits set from an option and the selection rather than selected directly.
    pub derived: &'static [Derived],
    /// Weapon classes, as the game interns them.
    pub classes: &'static [Class],
    /// Every hook site.
    pub sites: &'static [Site],
    /// The default binding of the master switch.
    pub master_default: BindingDefault,
    /// The settings schema version this profile writes. Every `since` lies in `1..=settings_schema`.
    pub settings_schema: u32,
}

impl Profile {
    /// Most toggles a profile may declare: a [`Selection`] has one bit per toggle.
    pub const MAX_TOGGLES: usize = 64;
    /// Most options a profile may declare: an [`OptionSet`] has one bit per option.
    pub const MAX_OPTIONS: usize = 16;
    /// Longest label of a toggle, option or class, in characters.
    pub const MAX_LABEL: usize = 24;
    /// Longest hint, group title or game name, in characters.
    pub const MAX_HINT: usize = 72;
    /// Toggle and option keys a profile cannot use: the settings file and trainer use them.
    pub const RESERVED_KEYS: [&'static str; 3] = ["master", "sounds", "start_paused"];
    /// Group keys a profile cannot use: the settings file uses them for its own tables.
    pub const RESERVED_GROUPS: [&'static str; 4] = ["trainer", "options", "bindings", "schema"];

    /// Fails the build if the profile breaks an invariant: `const _: () = PROFILE.check();`.
    ///
    /// ```
    /// use edbfi_trainer::profile::synthetic::SYNTHETIC;
    /// const _: () = SYNTHETIC.check();
    /// ```
    ///
    /// A reserved group key, a context shorter than 16 bytes, or a duplicate default binding each fail the
    /// build:
    ///
    /// ```compile_fail
    /// use edbfi_trainer::profile::{Group, Profile, synthetic::SYNTHETIC};
    /// const BAD: Profile = Profile {
    ///     groups: &[Group { key: "schema", title: "Cheats" }, SYNTHETIC.groups[1]],
    ///     ..SYNTHETIC
    /// };
    /// const _: () = BAD.check();
    /// ```
    ///
    /// ```compile_fail
    /// use edbfi_trainer::profile::{Context, Profile, Site, synthetic::SYNTHETIC};
    /// use edbfi_trainer::addr::Rva;
    /// const S1: Site = Site {
    ///     context: Context { rva: Rva::new(0x1100), aob: edbfi_trainer::aob!("48 89 5C 24 08 57") },
    ///     ..SYNTHETIC.sites[0]
    /// };
    /// const BAD: Profile = Profile {
    ///     sites: &[S1, SYNTHETIC.sites[1], SYNTHETIC.sites[2], SYNTHETIC.sites[3]],
    ///     ..SYNTHETIC
    /// };
    /// const _: () = BAD.check();
    /// ```
    ///
    /// ```compile_fail
    /// use edbfi_trainer::profile::{Profile, synthetic::SYNTHETIC};
    /// const BAD: Profile = Profile { master_default: SYNTHETIC.toggles[0].binding, ..SYNTHETIC };
    /// const _: () = BAD.check();
    /// ```
    ///
    /// # Panics
    ///
    /// With the error's message if [`Profile::validate`] fails; in a constant this is a build error. Call
    /// [`Profile::validate`] from a test to see which entry is at fault.
    pub const fn check(&self) {
        if let Err(error) = self.validate() {
            error.fail();
        }
    }
}

/// A game's names.
#[derive(Clone, Copy, Debug)]
pub struct Game {
    /// The display name, such as `Metro 2033 Redux`.
    pub name: &'static str,
    /// The executable's file name, matched without regard to ASCII case.
    pub exe: &'static str,
}

/// A mod's repository folders, `<game>/<mod>`. Settings live in `%APPDATA%\edbfi\<game>\<mod>\`.
#[derive(Clone, Copy, Debug)]
pub struct ModSlug {
    /// The game folder, kebab-case.
    pub game: &'static str,
    /// The mod folder, kebab-case.
    pub r#mod: &'static str,
}

/// How a supported build is recognised.
#[derive(Clone, Copy, Debug)]
pub struct Identity {
    /// SHA-256 of the executable file.
    pub disk_sha256: Sha256Digest,
    /// The code section once unpacked.
    pub text: TextSection,
}

/// The loaded code section that every hook site lies in.
#[derive(Clone, Copy, Debug)]
pub struct TextSection {
    /// Where it starts.
    pub rva: Rva,
    /// Its size in bytes.
    pub size: u32,
    /// SHA-256 of its bytes once relocations are undone.
    pub sha256: Sha256Digest,
}

impl TextSection {
    /// Whether `[rva, rva + len)` lies inside the section.
    #[must_use]
    pub const fn contains(&self, rva: Rva, len: u32) -> bool {
        let (start, end) = (
            self.rva.get() as u64,
            self.rva.get() as u64 + self.size as u64,
        );
        let (first, last) = (rva.get() as u64, rva.get() as u64 + len as u64);
        start <= first && last <= end
    }
}

/// Pointer chains from the image to the player, and offsets inside a weapon. Any null on a chain takes the
/// game's original path.
#[derive(Clone, Copy, Debug)]
pub struct PlayerLayout {
    /// The global holding the actor manager: `manager = *(base + manager)`.
    pub manager: Rva,
    /// `player = *(manager + player)`.
    pub player: u32,
    /// The player's owner identity is the address `player + owner`.
    pub owner: u32,
    /// A weapon's owner handle: `*(weapon + weapon_owner)` is its owner's identity.
    pub weapon_owner: u32,
    /// A weapon's configuration: `config = *(weapon + weapon_config)`; its first DWORD is the class id.
    pub weapon_config: u32,
    /// A weapon's current pressure, an `f32` at `weapon + weapon_pressure`.
    pub weapon_pressure: u32,
    /// The normal pressure, an `f32` at `config + config_normal_pressure`.
    pub config_normal_pressure: u32,
    /// The key of the class whose pressure the pressure sites keep. Required if any site is a pressure site.
    pub pressure_class: &'static str,
}

/// A dashboard section.
#[derive(Clone, Copy, Debug)]
pub struct Group {
    /// The settings key; independent of the title, so the title can change.
    pub key: &'static str,
    /// The heading.
    pub title: &'static str,
}

/// How far a cheat has been checked in gameplay. Shown next to the cheat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Checked {
    /// It worked in a gameplay check.
    Passed,
    /// Part of it worked in a gameplay check; the rest is unconfirmed.
    Partial,
    /// Not yet checked in gameplay.
    Pending,
}

/// Something a user can select.
#[derive(Clone, Copy, Debug)]
pub struct Toggle {
    /// The settings key, `[a-z0-9][a-z0-9_]*`. Never renamed without a settings migration.
    pub key: &'static str,
    /// The row label.
    pub label: &'static str,
    /// One line saying what it does.
    pub hint: &'static str,
    /// The [`Group`] key it is shown under.
    pub group: &'static str,
    /// The settings schema that introduced this selection.
    pub since: u32,
    /// The control bits it turns on.
    pub controls: ControlSet,
    /// Its default binding.
    pub binding: BindingDefault,
    /// Whether it is selected on first run.
    pub default: bool,
    /// Its gameplay check status.
    pub gameplay: Checked,
}

/// A default binding and the settings schema that introduced it.
#[derive(Clone, Copy, Debug)]
pub struct BindingDefault {
    /// The settings schema that introduced this default.
    pub since: u32,
    /// The chord, or `None` for unbound.
    pub default: Option<Chord>,
}

/// A game setting that is not a cheat, such as keeping a weapon's pressure.
#[derive(Clone, Copy, Debug)]
pub struct GameOption {
    /// The settings key, `[a-z0-9][a-z0-9_]*`.
    pub key: &'static str,
    /// The row label.
    pub label: &'static str,
    /// One line saying what it does.
    pub hint: &'static str,
    /// The settings schema that introduced it.
    pub since: u32,
    /// Its value on first run.
    pub default: bool,
}

/// A bit the trainer sets itself: on when `option` is on and any toggle in `when_any` is selected.
#[derive(Clone, Copy, Debug)]
pub struct Derived {
    /// The bit. No toggle selects it directly.
    pub bit: ControlBit,
    /// The [`GameOption`] key that enables it.
    pub option: &'static str,
    /// [`Toggle`] keys; selecting any of them sets the bit.
    pub when_any: &'static [&'static str],
}

/// A weapon class as the game interns it.
#[derive(Clone, Copy, Debug)]
pub struct Class {
    /// The settings key of its toggle, `[a-z0-9][a-z0-9_]*`.
    pub key: &'static str,
    /// The row label.
    pub label: &'static str,
    /// The settings schema that introduced it.
    pub since: u32,
    /// The control bit selecting it.
    pub bit: ControlBit,
    /// Globals holding the class's interned ids. A weapon belongs to the class when its configuration's first
    /// DWORD equals the DWORD at any of them.
    pub interned_ids: &'static [Rva],
}

/// One toggle per class, in class order: the class's key, label, `since` and bit, unbound and unselected.
#[must_use]
pub const fn class_toggles<const N: usize>(
    classes: &[Class; N],
    group: &'static str,
    hint: &'static str,
    gameplay: Checked,
) -> [Toggle; N] {
    const BLANK: Toggle = Toggle {
        key: "",
        label: "",
        hint: "",
        group: "",
        since: 0,
        controls: ControlSet::EMPTY,
        binding: BindingDefault {
            since: 0,
            default: None,
        },
        default: false,
        gameplay: Checked::Pending,
    };
    let mut toggles = [BLANK; N];
    let mut i = 0;
    while i < N {
        let class = &classes[i];
        toggles[i] = Toggle {
            key: class.key,
            label: class.label,
            hint,
            group,
            since: class.since,
            controls: ControlSet::of(class.bit),
            binding: BindingDefault {
                since: class.since,
                default: None,
            },
            default: false,
            gameplay,
        };
        i += 1;
    }
    toggles
}

/// `first` followed by `second`, as one array of length `N = A + B`.
///
/// # Panics
///
/// If `N != A + B` or both are empty; in a constant this is a build error.
#[must_use]
pub const fn concat<const A: usize, const B: usize, const N: usize>(
    first: [Toggle; A],
    second: [Toggle; B],
) -> [Toggle; N] {
    assert!(A + B == N && N > 0, "concat: N must be A + B, and not 0");
    let mut out = [if A > 0 { first[0] } else { second[0] }; N];
    let mut i = 0;
    while i < A {
        out[i] = first[i];
        i += 1;
    }
    while i < N {
        out[i] = second[i - A];
        i += 1;
    }
    out
}

/// A hook site's number within its profile, shown in checks and logs as `S<n>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SiteId(pub u8);

impl fmt::Display for SiteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "S{}", self.0)
    }
}

/// Where a hook goes, what it replaces and what it does.
#[derive(Clone, Copy, Debug)]
pub struct Site {
    /// The site's number.
    pub id: SiteId,
    /// Where the hook's jump is written.
    pub rva: Rva,
    /// The bytes the jump replaces: at least 5, whole instructions.
    pub original: &'static [u8],
    /// The signature around it.
    pub context: Context,
    /// What the hook does.
    pub kind: HookKind,
}

/// A signature that must match exactly once in the code section, at `rva`. It covers the site's original bytes
/// and agrees with them.
#[derive(Clone, Copy, Debug)]
pub struct Context {
    /// Where the signature starts.
    pub rva: Rva,
    /// The signature: at least 16 bytes.
    pub aob: Pattern<'static>,
}

/// The reviewed hook shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookKind {
    /// Runs the original unless `bit` is set, in which case it does `effect`.
    Gate {
        /// The control bit.
        bit: ControlBit,
        /// What happens when it is set.
        effect: GateEffect,
    },
    /// Keeps a selected class's loaded rounds when the player's weapon fires.
    NoReload(ReloadKind),
    /// Keeps the pressure class's pressure at its normal level while `bit` is set.
    Pressure {
        /// The derived control bit.
        bit: ControlBit,
        /// Where in the weapon's code the site is.
        kind: PressureKind,
    },
}

/// What a gate does when its bit is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateEffect {
    /// Returns `true` to the predicate's caller (`mov al,1; ret`).
    ReturnTrue,
    /// Keeps the load `mov rax,[r14]` and skips the subtraction after it.
    ReserveLoad,
    /// Calls `enabled`, the action without its consumption wrapper, instead of `callee`, with the same return
    /// address. The original is `call callee`.
    Throwable {
        /// The original call target.
        callee: Rva,
        /// The target when the bit is set.
        enabled: Rva,
    },
    /// Zeroes `xmm9` (`xorps xmm9,xmm9`) instead of multiplying it.
    ZeroXmm9,
    /// Skips the virtual call that damages the mask.
    SkipMaskCall,
    /// Skips the `and` that clears the used medkit.
    SkipMedkitAnd,
    /// Jumps to `target` instead of comparing.
    GodBranch {
        /// Where to continue.
        target: Rva,
    },
    /// Skips the call to `callee`. The original is `call callee`.
    SkipBatteryCall {
        /// The original call target.
        callee: Rva,
    },
}

/// The no-reload site shapes. Each names the register holding the weapon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReloadKind {
    /// The shared fire routine; the weapon is in R14.
    SharedR14,
    /// The direct callback; the weapon is in RCX. The relocated setter call returns into the hook's code.
    DirectCallbackRcx,
    /// The event callback; the weapon is in RCX.
    EventCallbackRcx,
    /// The Hellsing's fire routine; the weapon is in RDI.
    HellsingFireRdi,
}

impl ReloadKind {
    /// Whether the game returns into the hook's code, which must then outlive the trainer's detach.
    #[must_use]
    pub const fn returns_into_cave(self) -> bool {
        matches!(self, Self::DirectCallbackRcx)
    }
}

/// The pressure site shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PressureKind {
    /// Pressure recovery entry; the weapon is in RCX; the floor is applied before the original.
    RecoveryRcx,
    /// First consumption; the weapon is in RBX; the floor is applied to XMM1 after the original.
    Consumption1RbxXmm1,
    /// Second consumption; the weapon is in RBX; the floor is applied to XMM0 after the original.
    Consumption2RbxXmm0,
}
