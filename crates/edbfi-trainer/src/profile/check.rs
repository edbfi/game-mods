// SPDX-License-Identifier: AGPL-3.0-only
//! [`Profile::validate`]: every invariant a profile must hold, checked in a constant.
//!
//! Constant evaluation has no iterators, trait calls or formatting, so the checks are plain loops and the build
//! error is a fixed message per [`ProfileError`] variant. Tests call [`Profile::validate`] to see the entry.
#![forbid(unsafe_code)]

use crate::addr::Rva;
use crate::aob::Token;
use crate::chord::Refusal;
use crate::control::ControlBit;
use crate::recovery::RecoveryAction;

use super::{BindingDefault, GateEffect, HookKind, Profile, Site, SiteId, TextSection};

/// The shortest context signature.
const MIN_CONTEXT: usize = 16;
/// The shortest original: room for an `E9 rel32` jump.
const MIN_ORIGINAL: usize = 5;
/// Opcode of `call rel32`.
const CALL_REL32: u8 = 0xE8;

/// A profile invariant that does not hold. Every variant names the entry at fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    /// The game name or executable name is empty, too long or not printable ASCII.
    #[error("the game name or executable name is malformed")]
    Game,
    /// A slug part is not kebab-case.
    #[error("slug part {part:?} is not kebab-case")]
    Slug {
        /// The offending part.
        part: &'static str,
    },
    /// The settings schema is 0.
    #[error("the settings schema starts at 1")]
    Schema,
    /// The code section is empty or ends past 4 GiB.
    #[error("the code section is empty or ends past 4 GiB")]
    TextSection,
    /// A key is not `[a-z0-9][a-z0-9_]*`.
    #[error("key {key:?} is not [a-z0-9][a-z0-9_]*")]
    KeySyntax {
        /// The key.
        key: &'static str,
    },
    /// A key the settings file or trainer uses itself.
    #[error("key {key:?} is reserved")]
    ReservedKey {
        /// The key.
        key: &'static str,
    },
    /// Two groups, or two toggles, options or classes, share a key.
    #[error("key {key:?} is used twice")]
    DuplicateKey {
        /// The key.
        key: &'static str,
    },
    /// A label, hint or title is empty, too long or not printable ASCII.
    #[error("the text of {owner:?} is empty, too long or not printable ASCII")]
    Text {
        /// The key of the entry holding it.
        owner: &'static str,
    },
    /// More toggles or options than a selection can hold.
    #[error("too many toggles or options")]
    TooMany,
    /// A toggle names a group that does not exist.
    #[error("toggle {toggle:?} is in unknown group {group:?}")]
    UnknownGroup {
        /// The toggle.
        toggle: &'static str,
        /// The group it names.
        group: &'static str,
    },
    /// A group with no toggle.
    #[error("group {group:?} has no toggle")]
    EmptyGroup {
        /// The group.
        group: &'static str,
    },
    /// A `since` outside `1..=settings_schema`.
    #[error("{key:?} has a since outside 1..=settings_schema")]
    Since {
        /// The entry's key; `master` for the master binding.
        key: &'static str,
    },
    /// A default binding the chord policy refuses.
    #[error("the default binding of {key:?} is refused: {refusal}")]
    RefusedBinding {
        /// The entry's key; `master` for the master binding.
        key: &'static str,
        /// Why.
        refusal: Refusal,
    },
    /// Two default bindings share a chord.
    #[error("{first:?} and {second:?} share a default binding")]
    DuplicateBinding {
        /// The earlier entry.
        first: &'static str,
        /// The later entry.
        second: &'static str,
    },
    /// A toggle that drives no bit.
    #[error("toggle {toggle:?} drives no control bit")]
    NoControls {
        /// The toggle.
        toggle: &'static str,
    },
    /// Two toggles drive exactly the same bits.
    #[error("toggles {first:?} and {second:?} drive the same bits")]
    SameControls {
        /// The earlier toggle.
        first: &'static str,
        /// The later toggle.
        second: &'static str,
    },
    /// A derived rule names an option that does not exist.
    #[error("a derived bit names unknown option {option:?}")]
    UnknownOption {
        /// The name.
        option: &'static str,
    },
    /// A derived rule names a toggle that does not exist, or names none.
    #[error("a derived bit names unknown trigger {toggle:?}")]
    UnknownTrigger {
        /// The name; empty when the rule names no trigger.
        toggle: &'static str,
    },
    /// A bit with two roles: two derived rules, two classes, or a derived, class or gate bit used as another.
    #[error("control bit {} has two roles", bit.index())]
    BitClash {
        /// The bit.
        bit: ControlBit,
    },
    /// A bit no site reads: selecting it would do nothing.
    #[error("control bit {} is selectable but no site reads it", bit.index())]
    UnusedBit {
        /// The bit.
        bit: ControlBit,
    },
    /// A bit a site reads that no toggle or derived rule can set.
    #[error("control bit {} is read by a site but nothing sets it", bit.index())]
    UnownedBit {
        /// The bit.
        bit: ControlBit,
    },
    /// A class without exactly one toggle of the same key driving exactly its bit since the same schema.
    #[error("class {class:?} needs exactly one toggle of the same key, bit and since")]
    ClassToggle {
        /// The class.
        class: &'static str,
    },
    /// A class without interned ids, or an id shared by two classes.
    #[error("class {class:?} has no interned id or shares one")]
    ClassIds {
        /// The class.
        class: &'static str,
    },
    /// No-reload sites without classes, or classes without no-reload sites.
    #[error("no-reload sites and classes must come together")]
    Classes,
    /// Pressure sites whose pressure class is not a class.
    #[error("pressure class {class:?} is not a class")]
    PressureClass {
        /// The name.
        class: &'static str,
    },
    /// Two sites share an id.
    #[error("site {site} is declared twice")]
    DuplicateSite {
        /// The site.
        site: SiteId,
    },
    /// An original shorter than a jump.
    #[error("site {site}'s original is shorter than 5 bytes")]
    ShortOriginal {
        /// The site.
        site: SiteId,
    },
    /// A site, context or target outside the code section.
    #[error("site {site} reaches outside the code section")]
    OutsideText {
        /// The site.
        site: SiteId,
    },
    /// A context shorter than 16 bytes.
    #[error("site {site}'s context is shorter than 16 bytes")]
    ShortContext {
        /// The site.
        site: SiteId,
    },
    /// A context that does not cover the whole original.
    #[error("site {site}'s context does not cover its original")]
    ContextCoverage {
        /// The site.
        site: SiteId,
    },
    /// A context whose concrete byte differs from the original.
    #[error("site {site}'s context disagrees with its original at byte {offset}")]
    ContextDisagrees {
        /// The site.
        site: SiteId,
        /// Offset into the original.
        offset: usize,
    },
    /// Two sites' originals overlap.
    #[error("sites {first} and {second} overlap")]
    Overlap {
        /// The earlier site.
        first: SiteId,
        /// The later site.
        second: SiteId,
    },
    /// A call effect whose original is not `call rel32` to the declared callee.
    #[error("site {site}'s original is not a call to its declared callee")]
    CallTarget {
        /// The site.
        site: SiteId,
    },
}

impl ProfileError {
    /// Profiles are compiled in, so a broken one is a trainer bug.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        RecoveryAction::ReportBug
    }

    /// Panics with this error's fixed message: a build error in a constant.
    pub(super) const fn fail(self) -> ! {
        match self {
            Self::Game => panic!("profile: the game name or executable name is malformed"),
            Self::Slug { .. } => panic!("profile: a slug part is not kebab-case"),
            Self::Schema => panic!("profile: the settings schema starts at 1"),
            Self::TextSection => panic!("profile: the code section is empty or ends past 4 GiB"),
            Self::KeySyntax { .. } => panic!("profile: a key is not [a-z0-9][a-z0-9_]*"),
            Self::ReservedKey { .. } => panic!("profile: a key is reserved"),
            Self::DuplicateKey { .. } => panic!("profile: a key is used twice"),
            Self::Text { .. } => {
                panic!("profile: a label, hint or title is empty, too long or not printable ASCII")
            }
            Self::TooMany => panic!("profile: too many toggles or options"),
            Self::UnknownGroup { .. } => panic!("profile: a toggle is in an unknown group"),
            Self::EmptyGroup { .. } => panic!("profile: a group has no toggle"),
            Self::Since { .. } => panic!("profile: a since lies outside 1..=settings_schema"),
            Self::RefusedBinding { .. } => {
                panic!("profile: the chord policy refuses a default binding")
            }
            Self::DuplicateBinding { .. } => panic!("profile: two default bindings share a chord"),
            Self::NoControls { .. } => panic!("profile: a toggle drives no control bit"),
            Self::SameControls { .. } => panic!("profile: two toggles drive the same bits"),
            Self::UnknownOption { .. } => panic!("profile: a derived bit names an unknown option"),
            Self::UnknownTrigger { .. } => panic!("profile: a derived bit names an unknown toggle"),
            Self::BitClash { .. } => panic!("profile: a control bit has two roles"),
            Self::UnusedBit { .. } => {
                panic!("profile: a selectable control bit is read by no site")
            }
            Self::UnownedBit { .. } => panic!("profile: a site reads a control bit nothing sets"),
            Self::ClassToggle { .. } => {
                panic!("profile: a class needs exactly one toggle of the same key, bit and since")
            }
            Self::ClassIds { .. } => panic!("profile: a class has no interned id or shares one"),
            Self::Classes => panic!("profile: no-reload sites and classes must come together"),
            Self::PressureClass { .. } => panic!("profile: the pressure class is not a class"),
            Self::DuplicateSite { .. } => panic!("profile: a site id is declared twice"),
            Self::ShortOriginal { .. } => {
                panic!("profile: a site's original is shorter than 5 bytes")
            }
            Self::OutsideText { .. } => panic!("profile: a site reaches outside the code section"),
            Self::ShortContext { .. } => panic!("profile: a context is shorter than 16 bytes"),
            Self::ContextCoverage { .. } => {
                panic!("profile: a context does not cover its original")
            }
            Self::ContextDisagrees { .. } => {
                panic!("profile: a context disagrees with its original")
            }
            Self::Overlap { .. } => panic!("profile: two sites overlap"),
            Self::CallTarget { .. } => {
                panic!("profile: a call site's original is not a call to its declared callee")
            }
        }
    }
}

/// `Err(e)` out of a `Result<(), ProfileError>` in a constant, like `?`.
macro_rules! check {
    ($result:expr) => {
        if let Err(error) = $result {
            return Err(error);
        }
    };
}

impl Profile {
    /// Checks every invariant; [`Profile::check`] turns a failure into a build error.
    ///
    /// Run-time checks against the game (each context matches exactly once at its RVA) and the decoding of
    /// each original instruction belong to the trainer and its tests.
    ///
    /// # Errors
    ///
    /// The first [`ProfileError`] found.
    pub const fn validate(&self) -> Result<(), ProfileError> {
        check!(self.check_game());
        check!(self.check_groups());
        check!(self.check_toggles());
        check!(self.check_options());
        check!(self.check_bindings());
        check!(self.check_classes());
        check!(self.check_sites());
        self.check_bits()
    }

    const fn check_game(&self) -> Result<(), ProfileError> {
        if !text_ok(self.game.name, Self::MAX_HINT)
            || !text_ok(self.game.exe, Self::MAX_HINT)
            || contains_byte(self.game.exe, b'/')
            || contains_byte(self.game.exe, b'\\')
        {
            return Err(ProfileError::Game);
        }
        if !kebab(self.slug.game) {
            return Err(ProfileError::Slug {
                part: self.slug.game,
            });
        }
        if !kebab(self.slug.r#mod) {
            return Err(ProfileError::Slug {
                part: self.slug.r#mod,
            });
        }
        if self.settings_schema == 0 {
            return Err(ProfileError::Schema);
        }
        let text = &self.identity.text;
        if text.size == 0 || text.rva.get().checked_add(text.size).is_none() {
            return Err(ProfileError::TextSection);
        }
        Ok(())
    }

    const fn check_groups(&self) -> Result<(), ProfileError> {
        let groups = self.groups;
        let mut i = 0;
        while i < groups.len() {
            let group = &groups[i];
            check!(key_ok(group.key));
            if contains_str(&Self::RESERVED_GROUPS, group.key) {
                return Err(ProfileError::ReservedKey { key: group.key });
            }
            if !text_ok(group.title, Self::MAX_HINT) {
                return Err(ProfileError::Text { owner: group.key });
            }
            let mut j = 0;
            while j < i {
                if str_eq(groups[j].key, group.key) {
                    return Err(ProfileError::DuplicateKey { key: group.key });
                }
                j += 1;
            }
            let mut used = false;
            let mut t = 0;
            while t < self.toggles.len() {
                used |= str_eq(self.toggles[t].group, group.key);
                t += 1;
            }
            if !used {
                return Err(ProfileError::EmptyGroup { group: group.key });
            }
            i += 1;
        }
        Ok(())
    }

    /// Toggle and option keys share one namespace, so a settings key always means one entry.
    const fn check_key(
        &self,
        key: &'static str,
        toggles: usize,
        options: usize,
    ) -> Result<(), ProfileError> {
        check!(key_ok(key));
        if contains_str(&Self::RESERVED_KEYS, key) {
            return Err(ProfileError::ReservedKey { key });
        }
        let mut i = 0;
        while i < toggles {
            if str_eq(self.toggles[i].key, key) {
                return Err(ProfileError::DuplicateKey { key });
            }
            i += 1;
        }
        i = 0;
        while i < options {
            if str_eq(self.options[i].key, key) {
                return Err(ProfileError::DuplicateKey { key });
            }
            i += 1;
        }
        Ok(())
    }

    const fn check_since(&self, key: &'static str, since: u32) -> Result<(), ProfileError> {
        if since == 0 || since > self.settings_schema {
            Err(ProfileError::Since { key })
        } else {
            Ok(())
        }
    }

    const fn check_toggles(&self) -> Result<(), ProfileError> {
        let toggles = self.toggles;
        if toggles.len() > Self::MAX_TOGGLES || self.options.len() > Self::MAX_OPTIONS {
            return Err(ProfileError::TooMany);
        }
        let mut i = 0;
        while i < toggles.len() {
            let toggle = &toggles[i];
            check!(self.check_key(toggle.key, i, 0));
            if !text_ok(toggle.label, Self::MAX_LABEL) || !text_ok(toggle.hint, Self::MAX_HINT) {
                return Err(ProfileError::Text { owner: toggle.key });
            }
            let mut found = false;
            let mut g = 0;
            while g < self.groups.len() {
                found |= str_eq(self.groups[g].key, toggle.group);
                g += 1;
            }
            if !found {
                return Err(ProfileError::UnknownGroup {
                    toggle: toggle.key,
                    group: toggle.group,
                });
            }
            check!(self.check_since(toggle.key, toggle.since));
            check!(self.check_since(toggle.key, toggle.binding.since));
            if toggle.controls.is_empty() {
                return Err(ProfileError::NoControls { toggle: toggle.key });
            }
            let mut j = 0;
            while j < i {
                if toggles[j].controls.word() == toggle.controls.word() {
                    return Err(ProfileError::SameControls {
                        first: toggles[j].key,
                        second: toggle.key,
                    });
                }
                j += 1;
            }
            i += 1;
        }
        Ok(())
    }

    const fn check_options(&self) -> Result<(), ProfileError> {
        let options = self.options;
        let mut i = 0;
        while i < options.len() {
            let option = &options[i];
            check!(self.check_key(option.key, self.toggles.len(), i));
            if !text_ok(option.label, Self::MAX_LABEL) || !text_ok(option.hint, Self::MAX_HINT) {
                return Err(ProfileError::Text { owner: option.key });
            }
            check!(self.check_since(option.key, option.since));
            i += 1;
        }
        let derived = self.derived;
        i = 0;
        while i < derived.len() {
            let rule = &derived[i];
            if self.option_index(rule.option).is_none() {
                return Err(ProfileError::UnknownOption {
                    option: rule.option,
                });
            }
            if rule.when_any.is_empty() {
                return Err(ProfileError::UnknownTrigger { toggle: "" });
            }
            let mut t = 0;
            while t < rule.when_any.len() {
                if self.toggle_index(rule.when_any[t]).is_none() {
                    return Err(ProfileError::UnknownTrigger {
                        toggle: rule.when_any[t],
                    });
                }
                t += 1;
            }
            i += 1;
        }
        Ok(())
    }

    const fn check_bindings(&self) -> Result<(), ProfileError> {
        check!(self.check_since("master", self.master_default.since));
        check!(binding_ok("master", self.master_default));
        let toggles = self.toggles;
        let mut i = 0;
        while i < toggles.len() {
            let (key, binding) = (toggles[i].key, toggles[i].binding);
            check!(binding_ok(key, binding));
            if let Some(chord) = binding.default {
                if let Some(master) = self.master_default.default
                    && master.same(chord)
                {
                    return Err(ProfileError::DuplicateBinding {
                        first: "master",
                        second: key,
                    });
                }
                let mut j = 0;
                while j < i {
                    if let Some(other) = toggles[j].binding.default
                        && other.same(chord)
                    {
                        return Err(ProfileError::DuplicateBinding {
                            first: toggles[j].key,
                            second: key,
                        });
                    }
                    j += 1;
                }
            }
            i += 1;
        }
        Ok(())
    }

    const fn check_classes(&self) -> Result<(), ProfileError> {
        let classes = self.classes;
        let mut i = 0;
        while i < classes.len() {
            let class = &classes[i];
            check!(key_ok(class.key));
            if !text_ok(class.label, Self::MAX_LABEL) {
                return Err(ProfileError::Text { owner: class.key });
            }
            check!(self.check_since(class.key, class.since));
            if class.interned_ids.is_empty() {
                return Err(ProfileError::ClassIds { class: class.key });
            }
            let mut j = 0;
            while j < i {
                if str_eq(classes[j].key, class.key) {
                    return Err(ProfileError::DuplicateKey { key: class.key });
                }
                if classes[j].bit.index() == class.bit.index() {
                    return Err(ProfileError::BitClash { bit: class.bit });
                }
                j += 1;
            }
            let mut id = 0;
            while id < class.interned_ids.len() {
                if self.id_seen(i, id) {
                    return Err(ProfileError::ClassIds { class: class.key });
                }
                id += 1;
            }
            let mut owners = 0;
            let mut t = 0;
            while t < self.toggles.len() {
                let toggle = &self.toggles[t];
                if str_eq(toggle.key, class.key) {
                    owners += 1;
                    if toggle.controls.word() != class.bit.word() || toggle.since != class.since {
                        return Err(ProfileError::ClassToggle { class: class.key });
                    }
                }
                t += 1;
            }
            if owners != 1 {
                return Err(ProfileError::ClassToggle { class: class.key });
            }
            i += 1;
        }
        let pressure_class = self.player.pressure_class;
        if self.sites_have_pressure() && self.class_index(pressure_class).is_none() {
            return Err(ProfileError::PressureClass {
                class: pressure_class,
            });
        }
        Ok(())
    }

    /// Whether interned id `id` of class `class` already appeared earlier.
    const fn id_seen(&self, class: usize, id: usize) -> bool {
        let rva = self.classes[class].interned_ids[id].get();
        let mut c = 0;
        while c <= class {
            let ids = self.classes[c].interned_ids;
            let end = if c == class { id } else { ids.len() };
            let mut k = 0;
            while k < end {
                if ids[k].get() == rva {
                    return true;
                }
                k += 1;
            }
            c += 1;
        }
        false
    }

    const fn check_sites(&self) -> Result<(), ProfileError> {
        let sites = self.sites;
        let text = &self.identity.text;
        let mut i = 0;
        while i < sites.len() {
            let site = &sites[i];
            check!(check_site(site, text));
            let mut j = 0;
            while j < i {
                let other = &sites[j];
                if other.id.0 == site.id.0 {
                    return Err(ProfileError::DuplicateSite { site: site.id });
                }
                let (a, b) = (site.rva.get() as u64, other.rva.get() as u64);
                if a < b + other.original.len() as u64 && b < a + site.original.len() as u64 {
                    return Err(ProfileError::Overlap {
                        first: other.id,
                        second: site.id,
                    });
                }
                j += 1;
            }
            i += 1;
        }
        Ok(())
    }

    /// The bit roles: gate and class bits are selected by toggles, derived bits are set by rules and read by
    /// pressure sites. Every bit has one role, and every bit that can be set is read.
    const fn check_bits(&self) -> Result<(), ProfileError> {
        let (mut gate, mut pressure, mut reload) = (0u32, 0u32, false);
        let mut i = 0;
        while i < self.sites.len() {
            match self.sites[i].kind {
                HookKind::Gate { bit, .. } => gate |= bit.word(),
                HookKind::Pressure { bit, .. } => pressure |= bit.word(),
                HookKind::NoReload(_) => reload = true,
            }
            i += 1;
        }
        if reload != !self.classes.is_empty() {
            return Err(ProfileError::Classes);
        }
        let mut class = 0u32;
        i = 0;
        while i < self.classes.len() {
            class |= self.classes[i].bit.word();
            i += 1;
        }
        let mut derived = 0u32;
        i = 0;
        while i < self.derived.len() {
            let bit = self.derived[i].bit;
            if derived & bit.word() != 0 {
                return Err(ProfileError::BitClash { bit });
            }
            derived |= bit.word();
            i += 1;
        }
        let mut selectable = 0u32;
        i = 0;
        while i < self.toggles.len() {
            selectable |= self.toggles[i].controls.word();
            i += 1;
        }
        if let Some(bit) = lowest(gate & class) {
            return Err(ProfileError::BitClash { bit });
        }
        if let Some(bit) = lowest((gate | class | selectable) & derived) {
            return Err(ProfileError::BitClash { bit });
        }
        if let Some(bit) = lowest(selectable & !(gate | class) | derived & !pressure) {
            return Err(ProfileError::UnusedBit { bit });
        }
        if let Some(bit) = lowest((gate | class) & !selectable | pressure & !derived) {
            return Err(ProfileError::UnownedBit { bit });
        }
        Ok(())
    }

    const fn sites_have_pressure(&self) -> bool {
        let mut i = 0;
        while i < self.sites.len() {
            if let HookKind::Pressure { .. } = self.sites[i].kind {
                return true;
            }
            i += 1;
        }
        false
    }

    /// The position of the toggle named `key`.
    pub(super) const fn toggle_index(&self, key: &str) -> Option<usize> {
        let mut i = 0;
        while i < self.toggles.len() {
            if str_eq(self.toggles[i].key, key) {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// The position of the option named `key`.
    pub(super) const fn option_index(&self, key: &str) -> Option<usize> {
        let mut i = 0;
        while i < self.options.len() {
            if str_eq(self.options[i].key, key) {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// The position of the class named `key`.
    pub(super) const fn class_index(&self, key: &str) -> Option<usize> {
        let mut i = 0;
        while i < self.classes.len() {
            if str_eq(self.classes[i].key, key) {
                return Some(i);
            }
            i += 1;
        }
        None
    }
}

/// The lowest set bit of `word`.
const fn lowest(word: u32) -> Option<ControlBit> {
    if word == 0 {
        None
    } else {
        ControlBit::try_new(word.trailing_zeros() as u8)
    }
}

const fn check_site(site: &Site, text: &TextSection) -> Result<(), ProfileError> {
    let id = site.id;
    let len = site.original.len();
    if len < MIN_ORIGINAL {
        return Err(ProfileError::ShortOriginal { site: id });
    }
    let context = site.context.aob.tokens();
    if context.len() < MIN_CONTEXT {
        return Err(ProfileError::ShortContext { site: id });
    }
    if !text.contains(site.rva, len as u32)
        || !text.contains(site.context.rva, context.len() as u32)
    {
        return Err(ProfileError::OutsideText { site: id });
    }
    let (site_at, context_at) = (site.rva.get() as usize, site.context.rva.get() as usize);
    if site_at < context_at || site_at + len > context_at + context.len() {
        return Err(ProfileError::ContextCoverage { site: id });
    }
    let skip = site_at - context_at;
    let mut offset = 0;
    while offset < len {
        if let Token::Byte(byte) = context[skip + offset]
            && byte != site.original[offset]
        {
            return Err(ProfileError::ContextDisagrees { site: id, offset });
        }
        offset += 1;
    }
    if let HookKind::Gate { effect, .. } = site.kind {
        match effect {
            GateEffect::Throwable { callee, enabled } => {
                check!(target_ok(id, text, enabled));
                check!(call_ok(site, text, callee));
            }
            GateEffect::SkipBatteryCall { callee } => check!(call_ok(site, text, callee)),
            GateEffect::GodBranch { target } => check!(target_ok(id, text, target)),
            GateEffect::ReturnTrue
            | GateEffect::ReserveLoad
            | GateEffect::ZeroXmm9
            | GateEffect::SkipMaskCall
            | GateEffect::SkipMedkitAnd => {}
        }
    }
    Ok(())
}

const fn target_ok(site: SiteId, text: &TextSection, target: Rva) -> Result<(), ProfileError> {
    if text.contains(target, 1) {
        Ok(())
    } else {
        Err(ProfileError::OutsideText { site })
    }
}

/// The original must be exactly `call rel32` landing on `callee`.
const fn call_ok(site: &Site, text: &TextSection, callee: Rva) -> Result<(), ProfileError> {
    check!(target_ok(site.id, text, callee));
    let bytes = site.original;
    if bytes.len() != 5 || bytes[0] != CALL_REL32 {
        return Err(ProfileError::CallTarget { site: site.id });
    }
    let rel = i32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as i64;
    let lands = site.rva.get() as i64 + 5 + rel;
    if lands == callee.get() as i64 {
        Ok(())
    } else {
        Err(ProfileError::CallTarget { site: site.id })
    }
}

const fn binding_ok(key: &'static str, binding: BindingDefault) -> Result<(), ProfileError> {
    if let Some(chord) = binding.default
        && let Err(refusal) = chord.check_policy()
    {
        return Err(ProfileError::RefusedBinding { key, refusal });
    }
    Ok(())
}

const fn key_ok(key: &'static str) -> Result<(), ProfileError> {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes[0] == b'_' {
        return Err(ProfileError::KeySyntax { key });
    }
    let mut i = 0;
    while i < bytes.len() {
        if !matches!(bytes[i], b'a'..=b'z' | b'0'..=b'9' | b'_') {
            return Err(ProfileError::KeySyntax { key });
        }
        i += 1;
    }
    Ok(())
}

/// `[a-z0-9]+(-[a-z0-9]+)*`.
const fn kebab(part: &str) -> bool {
    let bytes = part.as_bytes();
    if bytes.is_empty() || bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
        return false;
    }
    let mut i = 0;
    while i < bytes.len() {
        let ok = match bytes[i] {
            b'a'..=b'z' | b'0'..=b'9' => true,
            b'-' => bytes[i - 1] != b'-',
            _ => false,
        };
        if !ok {
            return false;
        }
        i += 1;
    }
    true
}

/// Non-empty, at most `max` bytes, printable ASCII, and no leading or trailing space.
const fn text_ok(text: &str, max: usize) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() > max || bytes[0] == b' ' || bytes[bytes.len() - 1] == b' ' {
        return false;
    }
    let mut i = 0;
    while i < bytes.len() {
        if !matches!(bytes[i], 0x20..=0x7E) {
            return false;
        }
        i += 1;
    }
    true
}

const fn contains_byte(text: &str, byte: u8) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == byte {
            return true;
        }
        i += 1;
    }
    false
}

const fn contains_str(list: &[&str], key: &str) -> bool {
    let mut i = 0;
    while i < list.len() {
        if str_eq(list[i], key) {
            return true;
        }
        i += 1;
    }
    false
}

const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}
