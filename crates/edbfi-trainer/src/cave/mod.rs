// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hook caves: the code a hooked site jumps to, generated from a profile's [`Site`]s and proven before use.
//!
//! [`plan`] turns a profile into one [`Hook`] per site: the exact jump written over the site, and the cave it
//! lands on. Builders are pure. They take addresses and return bytes, and never touch a process.
//!
//! Every cave instruction comes from iced-x86's `CodeAssembler`, so the assembler chooses the encodings; what a
//! cave does is checked by its proof and by execution, never by comparing bytes. Original instructions are
//! decoded at the site's own address before they are emitted again, and any branch, call or RIP-relative form
//! a hook shape does not handle explicitly is refused rather than relocated. Absolute targets (the continuation
//! after the site, a callee, a branch target) sit in a literal pool after the code and are reached with
//! `jmp`/`push qword [rip+literal]`. Only the site jump has a fixed encoding: `E9 rel32` padded with `NOP`s.
//!
//! Before a builder returns, the cave's proof decodes it again and walks every path. It checks that:
//!
//! - every path reads the control word exactly once, with one aligned DWORD load;
//! - the cave restores every general register and the flags it changes, popping each saved value into the
//!   register that pushed it, and writes no memory but its own stack frame and, once at most, the hook's
//!   declared effect (the pressure floor's store);
//! - instructions copied from the game run with nothing of the cave's on the stack, so they see the
//!   registers, flags and stack the game had at the site;
//! - every exit is one the hook shape declares, with the stack as the game left it, or with exactly the
//!   original return address pushed for a call;
//! - the code never reaches the literal pool, and every literal is used;
//! - a cave contains a call exactly when its type says the game can return into it.
//!
//! The proof does not model SIMD registers or MXCSR; what the pressure caves do to them is shown by running
//! them.
//!
//! Each cave gets its own page of one allocation (see [`place`]). A cave typed [`ReturnsIntoCave`] holds a
//! call that returns into the cave, so it must outlive the hook it serves.
#![forbid(unsafe_code)]

mod emit;
mod gate;
pub mod place;
mod pressure;
mod proof;
mod reload;
#[cfg(test)]
mod tests;

pub use proof::ProofError;

use std::fmt;
use std::marker::PhantomData;
use std::num::NonZeroU32;

use crate::addr::{AddressError, CodeRange, Rel32, Va};
use crate::profile::{HookKind, Profile, Site, SiteId, ValidProfile};
use crate::recovery::RecoveryAction;

/// Bytes in one cave's page. A cave never exceeds it.
pub const PAGE: u32 = 4096;

/// Where a cave and the data it uses live in the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    image: Va,
    cave: Va,
    controls: Va,
}

impl Layout {
    /// The game image at `image`, the cave at `cave` and the control word at `controls`.
    ///
    /// # Errors
    ///
    /// [`HookError::UnalignedCave`] if `cave` does not start a page, or [`HookError::UnalignedControls`] if the
    /// control word is not 4-byte aligned: caves read it with one aligned load.
    pub(crate) const fn new(image: Va, cave: Va, controls: Va) -> Result<Self, HookError> {
        if !cave.get().is_multiple_of(PAGE as u64) {
            return Err(HookError::UnalignedCave(cave));
        }
        if !controls.get().is_multiple_of(4) {
            return Err(HookError::UnalignedControls(controls));
        }
        Ok(Self {
            image,
            cave,
            controls,
        })
    }

    /// The loaded image's base.
    #[must_use]
    pub(crate) const fn image(self) -> Va {
        self.image
    }

    /// The cave's first byte.
    #[must_use]
    pub(crate) const fn cave(self) -> Va {
        self.cave
    }

    /// The control word.
    #[must_use]
    pub(crate) const fn controls(self) -> Va {
        self.controls
    }
}

/// Marks a cave that the game never returns into: once its hook is restored, no thread can enter it again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoCaveReturn;

/// Marks a cave holding a call that returns into the cave. A thread inside the callee comes back to the cave
/// even after its hook is restored, so the cave must stay mapped until the game exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReturnsIntoCave;

/// Whether a cave type can be returned into.
pub trait Reentry: sealed::Sealed + fmt::Debug {
    /// `true` for [`ReturnsIntoCave`].
    const RETURNS_INTO_CAVE: bool;
}

impl Reentry for NoCaveReturn {
    const RETURNS_INTO_CAVE: bool = false;
}

impl Reentry for ReturnsIntoCave {
    const RETURNS_INTO_CAVE: bool = true;
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::NoCaveReturn {}
    impl Sealed for super::ReturnsIntoCave {}
}

/// An absolute address the cave's code reads from its literal pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Literal {
    offset: u32,
    value: u64,
}

impl Literal {
    /// Its offset from the start of the cave.
    #[must_use]
    pub const fn offset(self) -> u32 {
        self.offset
    }

    /// The address it holds.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.value
    }
}

/// How control leaves a cave.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Exit {
    /// Jumps to an address in the game with the stack as the game left it.
    Jump(Va),
    /// Returns to the hooked function's caller.
    Return,
    /// Enters `callee` as if the game had called it from the site: `ret` is pushed as its return address.
    TailCall {
        /// Where the callee is.
        callee: Callee,
        /// The return address pushed for it: the continuation after the site.
        ret: Va,
    },
}

/// The callee of an [`Exit::TailCall`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Callee {
    /// A fixed address.
    Fixed(Va),
    /// Read from game memory through the original call's operand, such as a virtual call.
    Indirect,
}

impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Jump(target) => write!(f, "jump to {target}"),
            Self::Return => f.write_str("return"),
            Self::TailCall {
                callee: Callee::Fixed(callee),
                ret,
            } => write!(f, "call {callee} returning to {ret}"),
            Self::TailCall {
                callee: Callee::Indirect,
                ret,
            } => write!(f, "indirect call returning to {ret}"),
        }
    }
}

/// A proven cave, ready to be written at its address.
///
/// Only the builders in this module make one, and only after its proof passed. `R` says whether the game can
/// return into it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cave<R> {
    bytes: Vec<u8>,
    range: CodeRange,
    code: CodeRange,
    literals: Vec<Literal>,
    exits: Vec<Exit>,
    reentry: PhantomData<R>,
}

impl<R: Reentry> Cave<R> {
    /// Where it is written.
    #[must_use]
    pub const fn address(&self) -> Va {
        self.range.start()
    }

    /// Its bytes: code, then the literal pool.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The whole cave, code and literals.
    #[must_use]
    pub const fn range(&self) -> CodeRange {
        self.range
    }

    /// The instructions alone, without the literal pool.
    #[must_use]
    pub const fn code(&self) -> CodeRange {
        self.code
    }

    /// The literal pool's entries, in address order.
    #[must_use]
    pub fn literals(&self) -> &[Literal] {
        &self.literals
    }

    /// Every way control leaves the cave, sorted.
    #[must_use]
    pub fn exits(&self) -> &[Exit] {
        &self.exits
    }

    /// Whether the game can return into this cave.
    #[must_use]
    pub const fn returns_into_cave(&self) -> bool {
        R::RETURNS_INTO_CAVE
    }
}

/// A cave of either reentry type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Built {
    /// The game never returns into it.
    Leaves(Cave<NoCaveReturn>),
    /// The game can return into it.
    ReturnsInto(Cave<ReturnsIntoCave>),
}

impl Built {
    /// Where it is written.
    #[must_use]
    pub const fn address(&self) -> Va {
        match self {
            Self::Leaves(cave) => cave.address(),
            Self::ReturnsInto(cave) => cave.address(),
        }
    }

    /// Its bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Leaves(cave) => cave.bytes(),
            Self::ReturnsInto(cave) => cave.bytes(),
        }
    }

    /// The whole cave.
    #[must_use]
    pub const fn range(&self) -> CodeRange {
        match self {
            Self::Leaves(cave) => cave.range(),
            Self::ReturnsInto(cave) => cave.range(),
        }
    }

    /// The instructions alone, without the literal pool.
    #[must_use]
    pub const fn code(&self) -> CodeRange {
        match self {
            Self::Leaves(cave) => cave.code(),
            Self::ReturnsInto(cave) => cave.code(),
        }
    }

    /// Every way control leaves the cave.
    #[must_use]
    pub fn exits(&self) -> &[Exit] {
        match self {
            Self::Leaves(cave) => cave.exits(),
            Self::ReturnsInto(cave) => cave.exits(),
        }
    }

    /// Whether the game can return into it.
    #[must_use]
    pub const fn returns_into_cave(&self) -> bool {
        matches!(self, Self::ReturnsInto(_))
    }
}

/// Builds and proves the cave for `site`.
///
/// # Errors
///
/// A [`HookError`] if the site's original bytes are not the instructions its hook shape expects, an address
/// does not fit, the cave would exceed a page, or its proof fails. With a validated profile and the trainer's
/// own allocation every one of these is a bug.
pub(crate) fn build(site: &Site, profile: &Profile, layout: Layout) -> Result<Built, HookError> {
    let at = layout
        .image
        .at(site.rva)
        .map_err(|source| HookError::Address {
            site: site.id,
            source,
        })?;
    let draft = match site.kind {
        HookKind::Gate { bit, effect } => gate::build(site, layout, at, bit, effect),
        HookKind::NoReload(kind) => reload::build(site, profile, layout, at, kind),
        HookKind::Pressure { bit, kind } => pressure::build(site, profile, layout, at, bit, kind),
    }
    .map_err(|fault| fault.at(site.id))?;
    match site.kind {
        HookKind::NoReload(kind) if kind.returns_into_cave() => {
            draft.prove(site.id, layout).map(Built::ReturnsInto)
        }
        _ => draft.prove(site.id, layout).map(Built::Leaves),
    }
}

/// The jump written over a site: `E9 rel32` to `cave`, then `NOP`s up to `len` bytes.
///
/// # Errors
///
/// [`HookError::ShortSite`] if `len` is under 5, or [`HookError::Address`] if `cave` is out of rel32 range.
pub(crate) fn site_jump(site: SiteId, at: Va, cave: Va, len: usize) -> Result<Vec<u8>, HookError> {
    let jump_len = Rel32::JMP_LEN as usize;
    if len < jump_len {
        return Err(HookError::ShortSite { site, len });
    }
    let rel = Rel32::from_site(at, cave).map_err(|source| HookError::Address { site, source })?;
    let mut bytes = rel.jmp_bytes().to_vec();
    bytes.resize(len, 0x90);
    Ok(bytes)
}

/// One site's hook: the jump written over it and the cave the jump lands on.
#[derive(Clone, Debug)]
pub struct Hook {
    site: &'static Site,
    range: CodeRange,
    jump: Vec<u8>,
    cave: Built,
}

impl Hook {
    /// The profile's site.
    #[must_use]
    pub const fn site(&self) -> &'static Site {
        self.site
    }

    /// The site's address in the game.
    #[must_use]
    pub const fn at(&self) -> Va {
        self.range.start()
    }

    /// The bytes the jump replaces.
    #[must_use]
    pub const fn original(&self) -> &'static [u8] {
        self.site.original
    }

    /// The jump, exactly as long as the original.
    #[must_use]
    pub fn jump(&self) -> &[u8] {
        &self.jump
    }

    /// The bytes the hook writes over, as a range.
    #[must_use]
    pub const fn range(&self) -> CodeRange {
        self.range
    }

    /// The cave.
    #[must_use]
    pub const fn cave(&self) -> &Built {
        &self.cave
    }
}

/// Every hook of a profile, with cave `i` on page `i` of one allocation.
#[derive(Clone, Debug)]
pub struct HookPlan {
    allocation: Va,
    hooks: Vec<Hook>,
}

impl HookPlan {
    /// The allocation's first byte.
    #[must_use]
    pub const fn allocation(&self) -> Va {
        self.allocation
    }

    /// The hooks, in profile order.
    #[must_use]
    pub fn hooks(&self) -> &[Hook] {
        &self.hooks
    }

    /// Whether any cave can be returned into.
    #[must_use]
    pub fn returns_into_cave(&self) -> bool {
        self.hooks.iter().any(|hook| hook.cave.returns_into_cave())
    }
}

/// `[start, start + len)`, for a `len` already known to be non-zero and at most a page.
fn code_range(start: Va, len: usize) -> Result<CodeRange, AddressError> {
    let len = u32::try_from(len)
        .ok()
        .and_then(NonZeroU32::new)
        .ok_or(AddressError::Overflow {
            base: start,
            offset: len as u64,
        })?;
    CodeRange::new(start, len)
}

/// The size of the allocation holding a profile's caves: one page per site.
#[must_use]
pub fn allocation_len(profile: &Profile) -> u64 {
    profile.sites.len() as u64 * u64::from(PAGE)
}

/// Builds every hook of `profile` for an image at `image`, caves from `allocation` and the control word at
/// `controls`.
///
/// The control word must lie at least 64 KiB from every constant a cave holds: the image globals the hooks
/// read, the continuations and the call targets, and immediates such as the float bits `0x7F80_0000` the
/// pressure caves compare against. The proof refuses any other constant that near it, so a closer control
/// word fails here.
///
/// # Errors
///
/// The first [`HookError`] of any site: an unaligned allocation or control word, an address that does not
/// fit, an original that is not its hook shape's reviewed instructions, or a cave that fails its proof.
pub fn plan(
    profile: &ValidProfile<'_>,
    image: Va,
    allocation: Va,
    controls: Va,
) -> Result<HookPlan, HookError> {
    let profile = profile.profile();
    let hooks = profile
        .sites
        .iter()
        .enumerate()
        .map(|(page, site)| {
            let address = |source| HookError::Address {
                site: site.id,
                source,
            };
            let cave = allocation
                .checked_add(page as u64 * u64::from(PAGE))
                .map_err(address)?;
            let at = image.at(site.rva).map_err(address)?;
            let cave = build(site, profile, Layout::new(image, cave, controls)?)?;
            let jump = site_jump(site.id, at, cave.address(), site.original.len())?;
            let range = code_range(at, jump.len()).map_err(address)?;
            Ok(Hook {
                site,
                range,
                jump,
                cave,
            })
        })
        .collect::<Result<_, HookError>>()?;
    Ok(HookPlan { allocation, hooks })
}

/// A cave that cannot be built.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HookError {
    /// The control word is not 4-byte aligned.
    #[error("the control word at {0} is not 4-byte aligned")]
    UnalignedControls(Va),
    /// A cave does not start a page.
    #[error("the cave at {0} does not start a page")]
    UnalignedCave(Va),
    /// A site is too short for its jump.
    #[error("{site}: the site has {len} bytes, too few for a 5-byte jump")]
    ShortSite {
        /// The site.
        site: SiteId,
        /// Its original length.
        len: usize,
    },
    /// An address does not fit.
    #[error("{site}: {source}")]
    Address {
        /// The site.
        site: SiteId,
        /// What did not fit.
        source: AddressError,
    },
    /// The original bytes are not the instructions the hook shape expects.
    #[error("{site}: the original bytes are not the reviewed instructions: {reason}")]
    Original {
        /// The site.
        site: SiteId,
        /// What differs.
        reason: &'static str,
    },
    /// A profile value the cave cannot encode, such as a player-layout offset past 2 GiB.
    #[error("{site}: {reason}")]
    Unencodable {
        /// The site.
        site: SiteId,
        /// What cannot be encoded.
        reason: &'static str,
    },
    /// The assembler refused the cave.
    #[error("{site}: the assembler failed: {message}")]
    Assembler {
        /// The site.
        site: SiteId,
        /// The assembler's message.
        message: String,
    },
    /// The cave is larger than a page.
    #[error("{site}: the cave is {len} bytes, more than a page")]
    TooLarge {
        /// The site.
        site: SiteId,
        /// The cave's length.
        len: usize,
    },
    /// The generated cave failed its proof.
    #[error("{site}: the cave failed its proof at offset {offset:#x}: {reason}")]
    Proof {
        /// The site.
        site: SiteId,
        /// The offending instruction's offset in the cave.
        offset: u32,
        /// What failed.
        reason: ProofError,
    },
}

impl HookError {
    /// A validated profile and the trainer's own allocation never fail to build, so every failure is a bug.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        RecoveryAction::ReportBug
    }
}
