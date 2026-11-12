// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hotkey chords: a virtual key plus an exact set of Ctrl, Alt and Shift.
//!
//! Chords are identified by Windows virtual-key codes, not scan codes. Settings spell them canonically, such as
//! `Ctrl+Alt+F1`, `Alt+Num5` or `None`.
#![forbid(unsafe_code)]

use std::fmt;
use std::str::FromStr;

use crate::recovery::RecoveryAction;

/// A key a chord can use, identified by its Windows virtual-key code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(u8);

const NAMED: [(&str, u8); 8] = [
    ("Space", 0x20),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("End", 0x23),
    ("Home", 0x24),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("Pause", 0x13),
];
const VK_DIGIT_0: u8 = 0x30;
const VK_LETTER_A: u8 = 0x41;
const VK_NUMPAD_0: u8 = 0x60;
const VK_F1: u8 = 0x70;

impl Key {
    /// The space bar.
    pub const SPACE: Self = Self(0x20);
    /// The Pause/Break key.
    pub const PAUSE: Self = Self(0x13);
    /// The Delete key.
    pub const DELETE: Self = Self(0x2E);
    /// The Page Up key.
    pub const PAGE_UP: Self = Self(0x21);
    /// The Page Down key.
    pub const PAGE_DOWN: Self = Self(0x22);
    /// The End key.
    pub const END: Self = Self(0x23);
    /// The Home key.
    pub const HOME: Self = Self(0x24);

    /// The letter key `A`..=`Z`.
    #[must_use]
    pub const fn letter(letter: u8) -> Option<Self> {
        if letter.is_ascii_uppercase() {
            Some(Self(VK_LETTER_A + (letter - b'A')))
        } else {
            None
        }
    }

    /// The top-row digit key `0`..=`9`.
    #[must_use]
    pub const fn digit(digit: u8) -> Option<Self> {
        if digit <= 9 {
            Some(Self(VK_DIGIT_0 + digit))
        } else {
            None
        }
    }

    /// The numeric keypad key `Num0`..=`Num9` (with NumLock on).
    #[must_use]
    pub const fn numpad(digit: u8) -> Option<Self> {
        if digit <= 9 {
            Some(Self(VK_NUMPAD_0 + digit))
        } else {
            None
        }
    }

    /// The function key `F1`..=`F24`.
    #[must_use]
    pub const fn function(n: u8) -> Option<Self> {
        if n >= 1 && n <= 24 {
            Some(Self(VK_F1 + n - 1))
        } else {
            None
        }
    }

    /// The key with virtual-key code `vk`, if chords support it.
    #[must_use]
    pub fn from_vk(vk: u8) -> Option<Self> {
        let key = Self(vk);
        key.name().is_some().then_some(key)
    }

    /// The Windows virtual-key code.
    #[must_use]
    pub const fn vk(self) -> u8 {
        self.0
    }

    /// Whether the key types a character: a letter, a top-row digit or Space.
    #[must_use]
    pub const fn is_typing(self) -> bool {
        matches!(self.0, 0x20 | 0x30..=0x39 | 0x41..=0x5A)
    }

    /// The canonical name, or `None` for an unsupported code.
    fn name(self) -> Option<KeyName> {
        let vk = self.0;
        match vk {
            0x30..=0x39 => Some(KeyName::Digit(vk - VK_DIGIT_0)),
            0x41..=0x5A => Some(KeyName::Letter(char::from(vk))),
            0x60..=0x69 => Some(KeyName::Numpad(vk - VK_NUMPAD_0)),
            0x70..=0x87 => Some(KeyName::Function(vk - VK_F1 + 1)),
            _ => NAMED
                .iter()
                .find(|(_, code)| *code == vk)
                .map(|(name, _)| KeyName::Named(name)),
        }
    }

    fn parse(token: &str) -> Option<Self> {
        if let Some((_, vk)) = NAMED.iter().find(|(name, _)| *name == token) {
            return Some(Self(*vk));
        }
        let number = |digits: &str| {
            (!digits.starts_with('0') || digits == "0").then(|| digits.parse::<u8>().ok())?
        };
        match token.as_bytes() {
            [letter @ b'A'..=b'Z'] => Self::letter(*letter),
            [digit @ b'0'..=b'9'] => Self::digit(digit - b'0'),
            [b'F', ..] => number(&token[1..]).and_then(Self::function),
            [b'N', b'u', b'm', digit @ b'0'..=b'9'] => Self::numpad(digit - b'0'),
            _ => None,
        }
    }
}

enum KeyName {
    Named(&'static str),
    Letter(char),
    Digit(u8),
    Numpad(u8),
    Function(u8),
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(KeyName::Named(name)) => f.write_str(name),
            Some(KeyName::Letter(letter)) => write!(f, "{letter}"),
            Some(KeyName::Digit(digit)) => write!(f, "{digit}"),
            Some(KeyName::Numpad(digit)) => write!(f, "Num{digit}"),
            Some(KeyName::Function(n)) => write!(f, "F{n}"),
            None => write!(f, "VK{:#04x}", self.0),
        }
    }
}

/// An exact set of held modifiers. Windows (Win) keys are never part of a chord.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Modifiers(u8);

impl Modifiers {
    /// No modifier.
    pub const NONE: Self = Self(0);
    /// Either Ctrl key.
    pub const CTRL: Self = Self(1);
    /// Either Alt key.
    pub const ALT: Self = Self(2);
    /// Either Shift key.
    pub const SHIFT: Self = Self(4);

    /// Both sets together.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every modifier in `other` is held.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether no modifier is held.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    const ORDER: [(Self, &'static str); 3] = [
        (Self::CTRL, "Ctrl"),
        (Self::ALT, "Alt"),
        (Self::SHIFT, "Shift"),
    ];
}

/// A key pressed while exactly `modifiers` are held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Chord {
    modifiers: Modifiers,
    key: Key,
}

impl Chord {
    /// `key` with exactly `modifiers` held.
    #[must_use]
    pub const fn new(modifiers: Modifiers, key: Key) -> Self {
        Self { modifiers, key }
    }

    /// The modifiers that must be held, no more and no fewer.
    #[must_use]
    pub const fn modifiers(self) -> Modifiers {
        self.modifiers
    }

    /// The base key.
    #[must_use]
    pub const fn key(self) -> Key {
        self.key
    }

    /// Whether the trainer accepts this chord as a binding. Usable in constants, so a profile's default
    /// bindings are checked at build time.
    ///
    /// # Errors
    ///
    /// The [`Refusal`] explaining why the chord would misbehave.
    pub const fn check_policy(self) -> Result<(), Refusal> {
        const CTRL_ALT: u8 = Modifiers::CTRL.0 | Modifiers::ALT.0;
        let (mods, vk) = (self.modifiers.0, self.key.0);
        let alt_only = mods == Modifiers::ALT.0;
        if (alt_only && (vk == Key::SPACE.0 || vk == VK_F1 + 3))
            || (mods == CTRL_ALT && vk == Key::DELETE.0)
            || (self.modifiers.contains(Modifiers::CTRL) && vk == Key::PAUSE.0)
        {
            return Err(Refusal::System);
        }
        if mods == Modifiers::CTRL.0 && vk == VK_LETTER_A + (b'C' - b'A') {
            return Err(Refusal::Interrupt);
        }
        if Modifiers::SHIFT.contains(self.modifiers) && self.key.is_typing() {
            return Err(Refusal::Typing);
        }
        if self.modifiers.is_empty() && matches!(vk, 0x21..=0x24) {
            return Err(Refusal::Navigation);
        }
        Ok(())
    }

    /// Whether both chords need the same key and modifiers. `==` for constants.
    #[must_use]
    pub const fn same(self, other: Self) -> bool {
        self.modifiers.0 == other.modifiers.0 && self.key.0 == other.key.0
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (modifier, name) in Modifiers::ORDER {
            if self.modifiers.contains(modifier) {
                write!(f, "{name}+")?;
            }
        }
        write!(f, "{}", self.key)
    }
}

impl FromStr for Chord {
    type Err = ChordError;

    /// Parses `Ctrl+Alt+Shift+Key`. Modifiers may come in any order but at most once each; [`Chord`]'s
    /// `Display` writes them as Ctrl, Alt, Shift. Use [`Binding`] to also accept `None`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut tokens: Vec<&str> = text.split('+').collect();
        let key_token = tokens.pop().unwrap_or_default();
        if text.is_empty()
            || tokens
                .iter()
                .chain([&key_token])
                .any(|token| token.is_empty())
        {
            return Err(ChordError::EmptyPart);
        }
        let mut modifiers = Modifiers::NONE;
        for token in tokens {
            let modifier = Modifiers::ORDER
                .iter()
                .find(|(_, name)| *name == token)
                .map(|(modifier, _)| *modifier)
                .ok_or_else(|| ChordError::UnknownModifier {
                    token: token.to_owned(),
                    suggestion: suggest(token),
                })?;
            if modifiers.contains(modifier) {
                return Err(ChordError::RepeatedModifier {
                    token: token.to_owned(),
                });
            }
            modifiers = modifiers.union(modifier);
        }
        if key_token == "None" {
            return Err(ChordError::NoneInChord);
        }
        let key = Key::parse(key_token).ok_or_else(|| ChordError::UnknownKey {
            token: key_token.to_owned(),
            suggestion: suggest(key_token),
        })?;
        Ok(Self { modifiers, key })
    }
}

/// The canonical spelling of a token that only differs in letter case, for an error hint.
fn suggest(token: &str) -> Option<String> {
    let names = Modifiers::ORDER
        .iter()
        .map(|(_, name)| (*name).to_owned())
        .chain(NAMED.iter().map(|(n, _)| (*n).to_owned()));
    let generated = (b'A'..=b'Z')
        .map(|c| char::from(c).to_string())
        .chain((1..=24).map(|n| format!("F{n}")))
        .chain((0..=9).map(|n| format!("Num{n}")))
        .chain(["None".to_owned()]);
    names
        .chain(generated)
        .find(|name| name.eq_ignore_ascii_case(token) && name != token)
}

/// A binding: a chord, or unbound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Binding(pub Option<Chord>);

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(chord) => chord.fmt(f),
            None => f.write_str("None"),
        }
    }
}

impl FromStr for Binding {
    type Err = ChordError;

    /// Parses a [`Chord`] or the word `None`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text == "None" {
            Ok(Self(None))
        } else {
            text.parse().map(|chord| Self(Some(chord)))
        }
    }
}

/// Text that is not a valid chord.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChordError {
    /// Empty text, or an empty part around a `+`.
    #[error("a binding needs a key, with optional Ctrl+, Alt+ and Shift+ in front")]
    EmptyPart,
    /// A part before the key that is not Ctrl, Alt or Shift.
    #[error("unknown modifier {token:?}{}", hint(suggestion.as_deref()))]
    UnknownModifier {
        /// The offending part.
        token: String,
        /// The canonical spelling, if only the letter case differs.
        suggestion: Option<String>,
    },
    /// The same modifier twice.
    #[error("modifier {token} appears twice")]
    RepeatedModifier {
        /// The repeated modifier.
        token: String,
    },
    /// `None` combined with modifiers.
    #[error("None stands alone; it cannot have modifiers")]
    NoneInChord,
    /// A key name outside the supported set.
    #[error("unknown key {token:?}{}", hint(suggestion.as_deref()))]
    UnknownKey {
        /// The offending part.
        token: String,
        /// The canonical spelling, if only the letter case differs.
        suggestion: Option<String>,
    },
}

fn hint(suggestion: Option<&str>) -> String {
    suggestion
        .map(|s| format!("; did you mean {s}?"))
        .unwrap_or_default()
}

impl ChordError {
    /// Bindings come from the settings file.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        RecoveryAction::EditSettings
    }
}

/// Why a well-formed chord cannot be a binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// Windows or the console acts on it first: Alt+Space, Alt+F4, Ctrl+Alt+Delete or Ctrl+Break.
    #[error("Windows uses this chord itself")]
    System,
    /// Ctrl+C interrupts the console.
    #[error("Ctrl+C stops the trainer")]
    Interrupt,
    /// It would fire while typing: a letter, digit or Space alone or with Shift.
    #[error("it would fire while typing; add Ctrl or Alt")]
    Typing,
    /// The dashboard uses it to move through lists.
    #[error("the dashboard uses this key for navigation; add a modifier")]
    Navigation,
}

impl Refusal {
    /// The user picks another chord.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        RecoveryAction::EditSettings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn chord(text: &str) -> Chord {
        text.parse().unwrap()
    }

    #[test]
    fn every_supported_key_round_trips() {
        let mut names = vec![
            "Space", "PageUp", "PageDown", "End", "Home", "Insert", "Delete", "Pause",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        names.extend((b'A'..=b'Z').map(|c| char::from(c).to_string()));
        names.extend((0..=9).map(|n| n.to_string()));
        names.extend((0..=9).map(|n| format!("Num{n}")));
        names.extend((1..=24).map(|n| format!("F{n}")));
        assert_eq!(names.len(), 8 + 26 + 10 + 10 + 24);
        for name in names {
            let parsed = chord(&name);
            assert_eq!(parsed.to_string(), name);
            assert_eq!(Key::from_vk(parsed.key().vk()), Some(parsed.key()));
        }
    }

    #[test]
    fn virtual_key_codes_match_windows() {
        let codes = [
            ("Space", 0x20),
            ("A", 0x41),
            ("Z", 0x5A),
            ("0", 0x30),
            ("Num0", 0x60),
            ("Num9", 0x69),
        ];
        for (name, vk) in codes.into_iter().chain([
            ("F1", 0x70),
            ("F24", 0x87),
            ("Pause", 0x13),
            ("Delete", 0x2E),
        ]) {
            assert_eq!(chord(name).key().vk(), vk, "{name}");
        }
        assert_eq!(Key::from_vk(0x88), None);
        assert_eq!(Key::from_vk(0x10), None);
    }

    #[test]
    fn modifiers_are_exact_and_written_canonically() {
        let parsed = chord("Shift+Alt+Ctrl+F1");
        assert_eq!(parsed.to_string(), "Ctrl+Alt+Shift+F1");
        assert_eq!(
            parsed.modifiers(),
            Modifiers::CTRL
                .union(Modifiers::ALT)
                .union(Modifiers::SHIFT)
        );
        assert_ne!(chord("Alt+F1"), chord("Ctrl+Alt+F1"));
        assert_eq!("None".parse::<Binding>(), Ok(Binding(None)));
        assert_eq!(Binding(Some(chord("Alt+1"))).to_string(), "Alt+1");
        assert_eq!(Binding(None).to_string(), "None");
    }

    #[test]
    fn grammar_errors_explain_themselves() {
        let error = |text: &str| text.parse::<Binding>().unwrap_err();
        for text in ["", "+", "Alt+", "+F1", "Alt++F1"] {
            assert_eq!(error(text), ChordError::EmptyPart, "{text:?}");
        }
        assert_eq!(error("Alt+None"), ChordError::NoneInChord);
        assert_eq!(
            error("Alt+Alt+F1"),
            ChordError::RepeatedModifier {
                token: "Alt".into()
            }
        );
        assert_eq!(
            error("Win+F1"),
            ChordError::UnknownModifier {
                token: "Win".into(),
                suggestion: None
            }
        );
        assert_eq!(
            error("ALT+F1").to_string(),
            r#"unknown modifier "ALT"; did you mean Alt?"#
        );
        assert_eq!(
            error("Alt+f1").to_string(),
            r#"unknown key "f1"; did you mean F1?"#
        );
        assert_eq!(
            error("none").to_string(),
            r#"unknown key "none"; did you mean None?"#
        );
        for text in ["F0", "F25", "F01", "Num10", "AA", "Tab", "Alt-F1"] {
            assert!(
                matches!(error(text), ChordError::UnknownKey { .. }),
                "{text:?}"
            );
        }
    }

    #[test]
    fn policy_refuses_system_typing_and_navigation_chords() {
        let refused = [
            ("Alt+Space", Refusal::System),
            ("Alt+F4", Refusal::System),
            ("Ctrl+Alt+Delete", Refusal::System),
            ("Ctrl+Pause", Refusal::System),
            ("Ctrl+Shift+Pause", Refusal::System),
            ("Ctrl+C", Refusal::Interrupt),
            ("R", Refusal::Typing),
            ("Shift+7", Refusal::Typing),
            ("Space", Refusal::Typing),
            ("PageDown", Refusal::Navigation),
            ("Home", Refusal::Navigation),
        ];
        for (text, refusal) in refused {
            assert_eq!(chord(text).check_policy(), Err(refusal), "{text}");
        }
        let accepted = [
            "Alt+F1",
            "Alt+F6",
            "Alt+F8",
            "Alt+1",
            "F5",
            "Num5",
            "Alt+Num0",
            "Ctrl+Shift+C",
            "Shift+Home",
        ];
        for text in accepted {
            assert_eq!(chord(text).check_policy(), Ok(()), "{text}");
        }
    }

    proptest! {
        #[test]
        fn display_parse_round_trips(vk in any::<u8>(), mods in 0u8..8) {
            if let Some(key) = Key::from_vk(vk) {
                let chord = Chord::new(Modifiers(mods), key);
                prop_assert_eq!(chord.to_string().parse::<Chord>(), Ok(chord));
            }
        }

        #[test]
        fn arbitrary_text_never_panics(text in ".{0,24}") {
            let _ = text.parse::<Binding>();
        }
    }
}
