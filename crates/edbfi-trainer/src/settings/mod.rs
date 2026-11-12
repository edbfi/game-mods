// SPDX-License-Identifier: AGPL-3.0-or-later
//! The user's saved choices: a strict TOML file the trainer reads, upgrades and rewrites itself.
//!
//! [`Settings`] holds what persists: the selection, the options, every binding, `sounds` and `start_paused`.
//! Runtime state such as the master switch's pause, the acknowledged mask or the game's PID never persists.
//!
//! The file is canonical TOML with one table per section, every key present:
//!
//! ```toml
//! schema = 1
//!
//! [trainer]
//! start_paused = false
//! sounds = true
//!
//! [cheats]
//! unlimited_ammo = false
//!
//! [options]
//! keep_hellsing_pressure = true
//!
//! [bindings]
//! master = "Alt+F8"
//! unlimited_ammo = "Alt+F1"
//! ```
//!
//! Reading is strict: a file of schema `k` must hold exactly the keys introduced in schemas `1..=k`, with the
//! right types and unique, allowed bindings. A file of an older schema gains only the newly introduced entries,
//! at their defaults; a new default binding whose chord the file already uses stays unbound. Nothing else ever
//! falls back to a default, so a deleted binding cannot silently return.
//!
//! [`Store`] owns the file on disk.
#![forbid(unsafe_code)]

mod parse;
mod store;
#[cfg(test)]
mod tests;

pub use parse::{Expected, Location, Problem, SettingsError};
pub(crate) use store::FILE_NAME;
pub use store::{External, Load, Reset, Saved, Store, StoreError};

use std::fmt;

use crate::chord::{Binding, Chord, Refusal};
use crate::control::Master;
use crate::profile::{OptionId, OptionSet, Selection, ToggleId, ToggleMap, ValidProfile};

/// The largest settings file the trainer reads.
pub const MAX_BYTES: usize = 64 * 1024;

/// Everything the trainer saves for one profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Whether the master switch starts paused when the trainer connects to the game.
    pub start_paused: bool,
    /// Whether a sound plays when the active cheats change.
    pub sounds: bool,
    /// The selected toggles.
    pub selection: Selection,
    /// The enabled options.
    pub options: OptionSet,
    /// The hotkeys.
    pub bindings: Bindings,
}

impl Settings {
    /// First-run settings: every toggle and option at its profile default, master active on connect, sounds on.
    #[must_use]
    pub fn defaults(profile: &ValidProfile<'_>) -> Self {
        let desired = profile.defaults();
        Self {
            start_paused: false,
            sounds: true,
            selection: desired.selection,
            options: desired.options,
            bindings: Bindings::defaults(profile),
        }
    }

    /// The master switch's state when the trainer connects to the game.
    #[must_use]
    pub const fn master_on_connect(&self) -> Master {
        if self.start_paused {
            Master::Paused
        } else {
            Master::Active
        }
    }

    /// Reads a settings file of `profile`.
    ///
    /// # Errors
    ///
    /// A [`SettingsError`] naming the first problem in file order.
    pub fn parse(profile: &ValidProfile<'_>, bytes: &[u8]) -> Result<Parsed, SettingsError> {
        parse::parse(profile, bytes)
    }

    /// The canonical file for these settings, in the profile's current schema.
    ///
    /// # Panics
    ///
    /// If the settings belong to another profile.
    #[must_use]
    pub fn to_toml(&self, profile: &ValidProfile<'_>) -> String {
        Canonical {
            settings: self,
            profile,
        }
        .to_string()
    }

    /// Every key of schema `schema`, as `(table, key)` in file order. The top-level `schema` key comes first,
    /// with an empty table name.
    pub fn keys<'p>(
        profile: &ValidProfile<'p>,
        schema: u32,
    ) -> impl Iterator<Item = (&'static str, &'static str)> + use<'p> {
        std::iter::once(("", "schema")).chain(
            fields(profile)
                .into_iter()
                .filter(move |field| field.since <= schema)
                .map(|field| (field.table, field.key)),
        )
    }
}

/// A settings file that was read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    /// The settings, upgraded to the current schema.
    pub settings: Settings,
    /// Set when the file was an older schema; save the settings to write the current one.
    pub upgrade: Option<Upgrade>,
}

/// What reading an older schema changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upgrade {
    /// The schema the file had.
    pub from: u32,
    /// New entries whose default chord the file already used for something else; they start unbound.
    pub unbound: Vec<Target>,
}

/// Something a binding triggers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// The master switch.
    Master,
    /// A toggle.
    Toggle(ToggleId),
}

impl Target {
    /// The target's settings key: `master` or the toggle's key.
    ///
    /// # Panics
    ///
    /// If the toggle belongs to another profile.
    #[must_use]
    pub fn key(self, profile: &ValidProfile<'_>) -> &'static str {
        match self {
            Self::Master => "master",
            Self::Toggle(id) => profile.toggle(id).key,
        }
    }
}

/// Every hotkey of one profile. No two targets share a chord, and every chord passes
/// [`Chord::check_policy`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bindings {
    master: Binding,
    toggles: ToggleMap<Binding>,
}

impl Bindings {
    /// The profile's default bindings.
    #[must_use]
    pub fn defaults(profile: &ValidProfile<'_>) -> Self {
        Self {
            master: Binding(profile.profile().master_default.default),
            toggles: profile.toggle_map(|_, toggle| Binding(toggle.binding.default)),
        }
    }

    /// The binding of `target`.
    ///
    /// # Panics
    ///
    /// If the toggle belongs to another profile.
    #[must_use]
    pub fn get(&self, target: Target) -> Binding {
        match target {
            Target::Master => self.master,
            Target::Toggle(id) => *self.toggles.get(id),
        }
    }

    /// Every target with its binding: the master first, then the toggles in display order.
    pub fn iter(&self) -> impl Iterator<Item = (Target, Binding)> {
        std::iter::once((Target::Master, self.master)).chain(
            self.toggles
                .iter()
                .map(|(id, binding)| (Target::Toggle(id), *binding)),
        )
    }

    /// The target bound to `chord`, if any.
    #[must_use]
    pub fn holder(&self, chord: Chord) -> Option<Target> {
        self.iter()
            .find(|(_, binding)| binding.0 == Some(chord))
            .map(|(target, _)| target)
    }

    /// Binds `target` to `binding`, returning its previous binding.
    ///
    /// # Errors
    ///
    /// [`AssignError::Refused`] if the policy refuses the chord, [`AssignError::Taken`] if another target has
    /// it. Nothing changes on error.
    ///
    /// # Panics
    ///
    /// If the toggle belongs to another profile.
    pub fn assign(&mut self, target: Target, binding: Binding) -> Result<Binding, AssignError> {
        if let Some(chord) = binding.0 {
            chord.check_policy()?;
            if let Some(holder) = self.holder_other_than(chord, target) {
                return Err(AssignError::Taken { by: holder });
            }
        }
        Ok(self.set(target, binding))
    }

    fn holder_other_than(&self, chord: Chord, target: Target) -> Option<Target> {
        self.iter()
            .find(|(holder, binding)| *holder != target && binding.0 == Some(chord))
            .map(|(holder, _)| holder)
    }

    fn set(&mut self, target: Target, binding: Binding) -> Binding {
        match target {
            Target::Master => std::mem::replace(&mut self.master, binding),
            Target::Toggle(id) => self.toggles.replace(id, binding),
        }
    }
}

/// Why [`Bindings::assign`] refused a chord.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AssignError {
    /// The binding policy refuses the chord.
    #[error(transparent)]
    Refused(#[from] Refusal),
    /// Another target already has the chord.
    #[error("another action already uses this chord")]
    Taken {
        /// The target that has it.
        by: Target,
    },
}

/// Where a value lives in the file.
#[derive(Clone, Copy, Debug)]
enum Slot {
    StartPaused,
    Sounds,
    Toggle(ToggleId),
    Option(OptionId),
    Binding(Target),
}

/// One key of the file.
#[derive(Clone, Copy, Debug)]
struct Field {
    table: &'static str,
    key: &'static str,
    since: u32,
    slot: Slot,
}

/// Every key of the current schema, in file order: `[trainer]`, one table per group, `[options]`, then
/// `[bindings]` with the master first.
fn fields(profile: &ValidProfile<'_>) -> Vec<Field> {
    let data = profile.profile();
    let mut fields = vec![
        Field {
            table: "trainer",
            key: "start_paused",
            since: 1,
            slot: Slot::StartPaused,
        },
        Field {
            table: "trainer",
            key: "sounds",
            since: 1,
            slot: Slot::Sounds,
        },
    ];
    for group in data.groups {
        fields.extend(
            profile
                .toggles()
                .filter(|(_, toggle)| toggle.group == group.key)
                .map(|(id, toggle)| Field {
                    table: group.key,
                    key: toggle.key,
                    since: toggle.since,
                    slot: Slot::Toggle(id),
                }),
        );
    }
    fields.extend(profile.options().map(|(id, option)| Field {
        table: "options",
        key: option.key,
        since: option.since,
        slot: Slot::Option(id),
    }));
    fields.push(Field {
        table: "bindings",
        key: "master",
        since: data.master_default.since,
        slot: Slot::Binding(Target::Master),
    });
    fields.extend(profile.toggles().map(|(id, toggle)| Field {
        table: "bindings",
        key: toggle.key,
        since: toggle.binding.since,
        slot: Slot::Binding(Target::Toggle(id)),
    }));
    fields
}

/// The canonical file text. Keys are `[a-z0-9_]` and bindings use the chord vocabulary, so nothing needs
/// quoting beyond the binding's double quotes.
struct Canonical<'a, 'p> {
    settings: &'a Settings,
    profile: &'a ValidProfile<'p>,
}

impl fmt::Display for Canonical<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { settings, profile } = *self;
        let data = profile.profile();
        writeln!(
            f,
            "# {} trainer settings. The trainer saves this file itself and does not keep comments.",
            data.game.name
        )?;
        writeln!(f, "schema = {}", data.settings_schema)?;
        let mut table = "";
        for field in fields(profile) {
            if field.table != table {
                write!(f, "\n[{}]\n", field.table)?;
                table = field.table;
            }
            let key = field.key;
            match field.slot {
                Slot::StartPaused => writeln!(f, "{key} = {}", settings.start_paused)?,
                Slot::Sounds => writeln!(f, "{key} = {}", settings.sounds)?,
                Slot::Toggle(id) => writeln!(f, "{key} = {}", settings.selection.contains(id))?,
                Slot::Option(id) => writeln!(f, "{key} = {}", settings.options.contains(id))?,
                Slot::Binding(target) => {
                    writeln!(f, "{key} = \"{}\"", settings.bindings.get(target))?;
                }
            }
        }
        Ok(())
    }
}
