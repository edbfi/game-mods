// SPDX-License-Identifier: AGPL-3.0-or-later
//! Byte signatures with wildcards ("array of bytes" patterns).
//!
//! Patterns are written as whitespace-separated tokens: two hex digits in either case, or `?`/`??` for any
//! byte. Profiles build them at compile time with [`aob!`](crate::aob!), so a malformed signature is a build
//! error rather than an attach-time surprise.
#![forbid(unsafe_code)]

use std::fmt;

use crate::recovery::RecoveryAction;

/// One position of a [`Pattern`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Token {
    /// Exactly this byte.
    Byte(u8),
    /// Any byte.
    Any,
}

impl Token {
    /// Whether `byte` satisfies this token.
    #[must_use]
    pub const fn matches(self, byte: u8) -> bool {
        match self {
            Self::Byte(expected) => expected == byte,
            Self::Any => true,
        }
    }
}

/// A non-empty signature with at least one concrete byte.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pattern<'a>(&'a [Token]);

impl<'a> Pattern<'a> {
    /// Wraps tokens produced by [`parse`].
    ///
    /// # Errors
    ///
    /// [`AobError::Empty`] or [`AobError::AllWildcards`].
    pub const fn new(tokens: &'a [Token]) -> Result<Self, AobError> {
        if tokens.is_empty() {
            return Err(AobError::Empty);
        }
        let mut i = 0;
        while i < tokens.len() {
            if let Token::Byte(_) = tokens[i] {
                return Ok(Self(tokens));
            }
            i += 1;
        }
        Err(AobError::AllWildcards)
    }

    /// The tokens, in order.
    #[must_use]
    pub const fn tokens(self) -> &'a [Token] {
        self.0
    }

    /// Number of bytes the pattern spans.
    #[must_use]
    #[expect(clippy::len_without_is_empty, reason = "a pattern is never empty")]
    pub const fn len(self) -> usize {
        self.0.len()
    }

    /// Whether the pattern matches `bytes`, which must be exactly as long as the pattern.
    #[must_use]
    pub fn matches(self, bytes: &[u8]) -> bool {
        bytes.len() == self.0.len()
            && self
                .0
                .iter()
                .zip(bytes)
                .all(|(token, &byte)| token.matches(byte))
    }

    /// Every offset in `haystack` where the pattern matches, in ascending order, overlaps included.
    pub fn find_all(self, haystack: &[u8]) -> impl Iterator<Item = usize> {
        haystack
            .windows(self.0.len())
            .enumerate()
            .filter(move |(_, window)| self.matches(window))
            .map(|(at, _)| at)
    }

    /// The only offset where the pattern matches.
    ///
    /// # Errors
    ///
    /// [`MatchError::Missing`] for no match, [`MatchError::Ambiguous`] for more than one.
    pub fn find_unique(self, haystack: &[u8]) -> Result<usize, MatchError> {
        let mut matches = self.find_all(haystack);
        let first = matches.next().ok_or(MatchError::Missing)?;
        match matches.next() {
            None => Ok(first),
            Some(second) => Err(MatchError::Ambiguous { first, second }),
        }
    }
}

impl fmt::Debug for Pattern<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pattern(\"{self}\")")
    }
}

impl fmt::Display for Pattern<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, token) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            match token {
                Token::Byte(byte) => write!(f, "{byte:02X}")?,
                Token::Any => f.write_str("??")?,
            }
        }
        Ok(())
    }
}

/// A malformed signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AobError {
    /// No tokens at all.
    #[error("signature is empty")]
    Empty,
    /// Only wildcards, which would match anywhere.
    #[error("signature has no concrete byte")]
    AllWildcards,
    /// A token that is neither two hex digits nor `?`/`??`.
    #[error("signature token {index} is not two hex digits or ??")]
    BadToken {
        /// Zero-based token position.
        index: usize,
    },
    /// A wildcard where only concrete bytes are allowed, as in [`bytes!`](crate::bytes!).
    #[error("token {index} is a wildcard, but these bytes must be concrete")]
    Wildcard {
        /// Zero-based token position.
        index: usize,
    },
    /// The destination does not have one slot per token.
    #[error("signature has {actual} tokens, expected {expected}")]
    WrongLength {
        /// Slots provided.
        expected: usize,
        /// Tokens found.
        actual: usize,
    },
}

impl AobError {
    /// Signatures are compiled into the profile, so a bad one is a trainer bug.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        RecoveryAction::ReportBug
    }
}

/// Where a signature failed to locate exactly one match.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MatchError {
    /// The signature occurs nowhere.
    #[error("signature not found")]
    Missing,
    /// The signature occurs at least twice.
    #[error("signature matches at both {first:#x} and {second:#x}")]
    Ambiguous {
        /// First match offset.
        first: usize,
        /// Second match offset.
        second: usize,
    },
}

const fn is_space(byte: u8) -> bool {
    byte.is_ascii_whitespace()
}

const fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Number of whitespace-separated tokens in `text`.
#[must_use]
pub const fn token_count(text: &str) -> usize {
    let bytes = text.as_bytes();
    let (mut count, mut i, mut in_token) = (0, 0, false);
    while i < bytes.len() {
        let space = is_space(bytes[i]);
        if !space && !in_token {
            count += 1;
        }
        in_token = !space;
        i += 1;
    }
    count
}

/// Parses `text` into exactly `N` tokens. Usable in constants; see [`aob!`](crate::aob!).
///
/// # Errors
///
/// [`AobError::WrongLength`] if `text` does not hold `N` tokens, [`AobError::BadToken`] for a malformed token,
/// and the [`Pattern::new`] errors for an empty or all-wildcard signature.
pub const fn parse<const N: usize>(text: &str) -> Result<[Token; N], AobError> {
    let actual = token_count(text);
    if actual != N {
        return Err(AobError::WrongLength {
            expected: N,
            actual,
        });
    }
    let bytes = text.as_bytes();
    let mut tokens = [Token::Any; N];
    let (mut i, mut index) = (0, 0);
    while i < bytes.len() {
        if is_space(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && !is_space(bytes[i]) {
            i += 1;
        }
        tokens[index] = match (i - start, bytes[start]) {
            (1, b'?') => Token::Any,
            (2, b'?') if bytes[start + 1] == b'?' => Token::Any,
            (2, high) => match (hex(high), hex(bytes[start + 1])) {
                (Some(high), Some(low)) => Token::Byte(high << 4 | low),
                _ => return Err(AobError::BadToken { index }),
            },
            _ => return Err(AobError::BadToken { index }),
        };
        index += 1;
    }
    match Pattern::new(&tokens) {
        Ok(_) => Ok(tokens),
        Err(error) => Err(error),
    }
}

/// Parses `text` into exactly `N` concrete bytes, in the same notation as [`parse`] but without wildcards.
/// Usable in constants; see [`bytes!`](crate::bytes!).
///
/// # Errors
///
/// [`AobError::Wildcard`] for a `?` or `??` token, otherwise as [`parse`].
pub const fn parse_concrete<const N: usize>(text: &str) -> Result<[u8; N], AobError> {
    let tokens = match parse::<N>(text) {
        Ok(tokens) => tokens,
        Err(error) => return Err(error),
    };
    let mut bytes = [0; N];
    let mut index = 0;
    while index < N {
        bytes[index] = match tokens[index] {
            Token::Byte(byte) => byte,
            Token::Any => return Err(AobError::Wildcard { index }),
        };
        index += 1;
    }
    Ok(bytes)
}

/// Fails a constant evaluation with a message naming the error; used by [`aob!`](crate::aob!).
#[doc(hidden)]
pub const fn __build_error(error: AobError) -> ! {
    match error {
        AobError::Empty => panic!("byte signature is empty"),
        AobError::AllWildcards => panic!("byte signature has no concrete byte"),
        AobError::BadToken { .. } => panic!("byte signature token is not two hex digits or ??"),
        AobError::Wildcard { .. } => panic!("concrete bytes cannot hold a wildcard"),
        AobError::WrongLength { .. } => panic!("byte signature token count changed during parsing"),
    }
}

/// A compile-time checked `Pattern<'static>`: `aob!("48 8B ?? 05")`.
///
/// A malformed signature fails the build:
///
/// ```compile_fail
/// const BAD: edbfi_trainer::aob::Pattern<'static> = edbfi_trainer::aob!("48 8G");
/// ```
///
/// ```compile_fail
/// const WILD: edbfi_trainer::aob::Pattern<'static> = edbfi_trainer::aob!("?? ??");
/// ```
#[macro_export]
macro_rules! aob {
    ($text:expr) => {{
        // Item names are not hygienic: prefixed so they cannot shadow a caller's constant.
        const __EDBFI_AOB_TEXT: &str = $text;
        const __EDBFI_AOB_TOKENS: [$crate::aob::Token; $crate::aob::token_count(__EDBFI_AOB_TEXT)] =
            match $crate::aob::parse(__EDBFI_AOB_TEXT) {
                Ok(tokens) => tokens,
                Err(error) => $crate::aob::__build_error(error),
            };
        match $crate::aob::Pattern::new(&__EDBFI_AOB_TOKENS) {
            Ok(pattern) => pattern,
            Err(error) => $crate::aob::__build_error(error),
        }
    }};
}

/// Compile-time checked concrete bytes, as a `&'static [u8]`: `bytes!("48 89 5C 24 08")`.
///
/// The notation is [`aob!`](crate::aob!)'s, so original bytes and their context read alike, but a wildcard
/// fails the build:
///
/// ```compile_fail
/// const BAD: &[u8] = edbfi_trainer::bytes!("48 ?? 5C");
/// ```
#[macro_export]
macro_rules! bytes {
    ($text:expr) => {{
        const __EDBFI_BYTES_TEXT: &str = $text;
        const __EDBFI_BYTES: [u8; $crate::aob::token_count(__EDBFI_BYTES_TEXT)] =
            match $crate::aob::parse_concrete(__EDBFI_BYTES_TEXT) {
                Ok(bytes) => bytes,
                Err(error) => $crate::aob::__build_error(error),
            };
        &__EDBFI_BYTES as &'static [u8]
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const SAMPLE: Pattern<'static> = crate::aob!("f3 44 ?? 59\tCA ?");
    const TEXT: &str = "48 8B";
    const TOKENS: Pattern<'static> = crate::aob!(TEXT);

    #[test]
    fn grammar_accepts_hex_either_case_and_both_wildcards() {
        assert_eq!(
            SAMPLE.tokens(),
            [
                Token::Byte(0xF3),
                Token::Byte(0x44),
                Token::Any,
                Token::Byte(0x59),
                Token::Byte(0xCA),
                Token::Any
            ]
        );
        assert_eq!(SAMPLE.to_string(), "F3 44 ?? 59 CA ??");
        assert_eq!(
            TOKENS.to_string(),
            "48 8B",
            "a caller's constant named like the macro's own items"
        );
    }

    #[test]
    fn grammar_rejects_malformed_tokens() {
        assert_eq!(parse::<0>(""), Err(AobError::Empty));
        assert_eq!(parse::<0>("  \n "), Err(AobError::Empty));
        assert_eq!(parse::<2>("?? ?"), Err(AobError::AllWildcards));
        assert_eq!(
            parse::<3>("48 8B"),
            Err(AobError::WrongLength {
                expected: 3,
                actual: 2
            })
        );
        for (text, index) in [
            ("4", 0),
            ("488B", 0),
            ("???", 0),
            ("?A", 0),
            ("0x", 0),
            ("48 8", 1),
            ("48 8G", 1),
        ] {
            let error = match token_count(text) {
                1 => parse::<1>(text).err(),
                2 => parse::<2>(text).err(),
                _ => unreachable!(),
            };
            assert_eq!(error, Some(AobError::BadToken { index }), "{text:?}");
        }
    }

    #[test]
    fn concrete_bytes_refuse_wildcards() {
        const BYTES: &[u8] = crate::bytes!("48 89\t5c 24 08");
        assert_eq!(BYTES, [0x48, 0x89, 0x5C, 0x24, 0x08]);
        assert_eq!(
            parse_concrete::<3>("48 ?? 5C"),
            Err(AobError::Wildcard { index: 1 })
        );
        assert_eq!(parse_concrete::<0>(""), Err(AobError::Empty));
        assert_eq!(
            parse_concrete::<1>("4G"),
            Err(AobError::BadToken { index: 0 })
        );
    }

    #[test]
    fn find_all_reports_overlapping_matches() {
        let pattern = crate::aob!("AA ?? AA");
        let haystack = [0xAA, 0x00, 0xAA, 0x01, 0xAA, 0xAA];
        assert_eq!(pattern.find_all(&haystack).collect::<Vec<_>>(), [0, 2]);
        assert_eq!(
            pattern.find_unique(&haystack),
            Err(MatchError::Ambiguous {
                first: 0,
                second: 2
            })
        );
        assert_eq!(pattern.find_unique(&haystack[1..]), Ok(1));
        assert_eq!(
            pattern.find_unique(&haystack[..2]),
            Err(MatchError::Missing)
        );
        assert_eq!(pattern.find_unique(&[]), Err(MatchError::Missing));
    }

    proptest! {
        #[test]
        fn display_round_trips(tokens in prop::collection::vec(prop_oneof![any::<u8>().prop_map(Token::Byte), Just(Token::Any)], 1..32)) {
            prop_assume!(tokens.iter().any(|t| matches!(t, Token::Byte(_))));
            let text = Pattern::new(&tokens).unwrap().to_string();
            prop_assert_eq!(token_count(&text), tokens.len());
            let mut parsed = vec![Token::Any; tokens.len()];
            for (slot, word) in parsed.iter_mut().zip(text.split(' ')) {
                *slot = match word {
                    "??" => Token::Any,
                    hex => Token::Byte(u8::from_str_radix(hex, 16).unwrap()),
                };
            }
            prop_assert_eq!(parsed, tokens);
        }

        #[test]
        fn find_all_agrees_with_a_naive_scan(needle in prop::collection::vec(any::<u8>(), 1..4), hay in prop::collection::vec(0u8..4, 0..64)) {
            let tokens: Vec<Token> = needle.iter().map(|&b| if b % 3 == 0 { Token::Any } else { Token::Byte(b % 4) }).collect();
            prop_assume!(tokens.iter().any(|t| matches!(t, Token::Byte(_))));
            let pattern = Pattern::new(&tokens).unwrap();
            let expected: Vec<usize> = (0..=hay.len().saturating_sub(tokens.len()))
                .filter(|&at| hay.len() >= at + tokens.len() && tokens.iter().zip(&hay[at..]).all(|(t, &b)| t.matches(b)))
                .collect();
            prop_assert_eq!(pattern.find_all(&hay).collect::<Vec<_>>(), expected);
        }
    }
}
