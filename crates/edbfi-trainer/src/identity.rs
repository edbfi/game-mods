// SPDX-License-Identifier: AGPL-3.0-or-later
//! Portable identity checks: file and code digests, loaded-header comparison and relocation normalisation.
//!
//! A game build is identified by the SHA-256 of its executable file. Because the supported executable unpacks
//! its code at startup, the code itself is verified separately: the loaded `.text` is copied out, every 64-bit
//! base relocation inside it is rebased to the preferred image base, and the result must hash to the profile's
//! compiled-in fingerprint.
#![forbid(unsafe_code)]

use std::fmt;

use sha2::{Digest as _, Sha256};

use crate::addr::{Rva, Va};
use crate::pe::{PeImage, Relocation};
use crate::recovery::RecoveryAction;

/// A SHA-256 digest.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    /// The digest of `data`.
    #[must_use]
    pub fn of(data: &[u8]) -> Self {
        Self(Sha256::digest(data).into())
    }

    /// Parses 64 hex digits, in either case. Usable in constants.
    ///
    /// # Panics
    ///
    /// If `hex` is not exactly 64 hex digits; in a constant this is a build error.
    #[must_use]
    pub const fn from_hex(hex: &str) -> Self {
        let hex = hex.as_bytes();
        assert!(hex.len() == 64, "a SHA-256 digest is 64 hex digits");
        let mut digest = [0; 32];
        let mut i = 0;
        while i < 32 {
            digest[i] = nibble(hex[2 * i]) << 4 | nibble(hex[2 * i + 1]);
            i += 1;
        }
        Self(digest)
    }

    /// The raw bytes.
    #[must_use]
    pub const fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

const fn nibble(digit: u8) -> u8 {
    match digit {
        b'0'..=b'9' => digit - b'0',
        b'a'..=b'f' => digit - b'a' + 10,
        b'A'..=b'F' => digit - b'A' + 10,
        _ => panic!("a SHA-256 digest is 64 hex digits"),
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256Digest({self})")
    }
}

/// Checks the loaded image's NT headers against the file's.
///
/// The loader changes exactly one field, `ImageBase`, which must hold the actual base. Every other byte must be
/// identical, which proves the loaded image comes from this file.
///
/// # Errors
///
/// [`IdentityError::HeaderLength`], [`IdentityError::ImageBase`] or [`IdentityError::HeaderMismatch`].
pub fn check_loaded_headers(
    disk: &PeImage<'_>,
    loaded: &[u8],
    actual_base: Va,
) -> Result<(), IdentityError> {
    let expected = disk.nt_headers();
    if loaded.len() != expected.len() {
        return Err(IdentityError::HeaderLength {
            expected: expected.len(),
            found: loaded.len(),
        });
    }
    let field = PeImage::NT_IMAGE_BASE_OFFSET..PeImage::NT_IMAGE_BASE_OFFSET + 8;
    let loaded_base = u64::from_le_bytes(loaded[field.clone()].try_into().map_err(|_| {
        IdentityError::HeaderLength {
            expected: expected.len(),
            found: loaded.len(),
        }
    })?);
    if loaded_base != actual_base.get() {
        return Err(IdentityError::ImageBase {
            header: Va::new(loaded_base),
            actual: actual_base,
        });
    }
    match expected
        .iter()
        .zip(loaded)
        .enumerate()
        .find(|(at, (a, b))| a != b && !field.contains(at))
    {
        Some((offset, _)) => Err(IdentityError::HeaderMismatch { offset }),
        None => Ok(()),
    }
}

/// Undoes the loader's base relocation of a code section copied from a running process.
///
/// `code` is the loaded section starting at `code_rva`. Every `DIR64` entry inside it is shifted back by
/// `actual_base - preferred_base`; `ABSOLUTE` entries are padding. The image is then byte-comparable with one
/// loaded at its preferred base.
///
/// # Errors
///
/// [`IdentityError::StraddlingRelocation`] for a `DIR64` crossing the section's edge, or
/// [`IdentityError::UnsupportedRelocation`] for any other entry type inside the section.
pub fn normalise_relocations(
    code: &mut [u8],
    code_rva: Rva,
    relocations: &[Relocation],
    preferred_base: u64,
    actual_base: Va,
) -> Result<(), IdentityError> {
    let delta = actual_base.get().wrapping_sub(preferred_base);
    let start = u64::from(code_rva.get());
    let end = start + code.len() as u64;
    for relocation in relocations {
        let at = u64::from(relocation.rva.get());
        match relocation.kind {
            Relocation::ABSOLUTE => {}
            Relocation::DIR64 if at + 8 <= start || at >= end => {}
            Relocation::DIR64 if at >= start && at + 8 <= end => {
                let offset = usize::try_from(at - start).map_err(|_| {
                    IdentityError::StraddlingRelocation {
                        rva: relocation.rva,
                    }
                })?;
                let slot = &mut code[offset..offset + 8];
                let value = u64::from_le_bytes(slot.try_into().map_err(|_| {
                    IdentityError::StraddlingRelocation {
                        rva: relocation.rva,
                    }
                })?);
                slot.copy_from_slice(&value.wrapping_sub(delta).to_le_bytes());
            }
            Relocation::DIR64 => {
                return Err(IdentityError::StraddlingRelocation {
                    rva: relocation.rva,
                });
            }
            kind if (start..end).contains(&at) => {
                return Err(IdentityError::UnsupportedRelocation {
                    rva: relocation.rva,
                    kind,
                });
            }
            _ => {}
        }
    }
    Ok(())
}

/// A running game whose image does not match the file it was loaded from, or the supported build.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    /// Loaded and file NT headers differ in size.
    #[error("loaded NT headers are {found} bytes, the file's are {expected}")]
    HeaderLength {
        /// Length in the file.
        expected: usize,
        /// Length read from the process.
        found: usize,
    },
    /// The loaded header's `ImageBase` is not where the image actually is.
    #[error("loaded ImageBase {header} differs from the actual base {actual}")]
    ImageBase {
        /// Value in the loaded header.
        header: Va,
        /// Where the image is loaded.
        actual: Va,
    },
    /// A loaded header byte other than `ImageBase` differs from the file.
    #[error("loaded NT headers differ from the file at byte {offset:#x}")]
    HeaderMismatch {
        /// First differing offset within the NT headers.
        offset: usize,
    },
    /// A 64-bit relocation crosses the edge of the verified section.
    #[error("relocation at {rva} straddles the code section's edge")]
    StraddlingRelocation {
        /// Where the relocated value starts.
        rva: Rva,
    },
    /// A relocation type the verifier cannot undo lies inside the code section.
    #[error("unsupported relocation type {kind} at {rva} inside the code section")]
    UnsupportedRelocation {
        /// Where it applies.
        rva: Rva,
        /// Its `IMAGE_REL_BASED_*` type.
        kind: u8,
    },
}

impl IdentityError {
    /// A loaded image that disagrees with its own file has been altered in memory; relocation shapes this
    /// verifier cannot handle mean an unsupported build.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        match self {
            Self::HeaderLength { .. } | Self::ImageBase { .. } | Self::HeaderMismatch { .. } => {
                RecoveryAction::ModifiedGameCode
            }
            Self::StraddlingRelocation { .. } | Self::UnsupportedRelocation { .. } => {
                RecoveryAction::UnsupportedBuild
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pe::testing::{block, image};
    use proptest::prelude::*;

    const PREFERRED: u64 = 0x1_4000_0000;

    #[test]
    fn digest_hex_round_trips_and_matches_a_known_vector() {
        let empty = Sha256Digest::of(b"");
        assert_eq!(
            empty.to_string(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        const UPPER: Sha256Digest = Sha256Digest::from_hex(
            "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855",
        );
        assert_eq!(UPPER, empty);
    }

    #[test]
    #[should_panic(expected = "64 hex digits")]
    fn digest_rejects_bad_hex() {
        let _ = Sha256Digest::from_hex(&"g".repeat(64));
    }

    fn loaded_headers(file: &[u8], base: u64) -> Vec<u8> {
        let mut loaded = PeImage::parse(file).unwrap().nt_headers().to_vec();
        let field = PeImage::NT_IMAGE_BASE_OFFSET;
        loaded[field..field + 8].copy_from_slice(&base.to_le_bytes());
        loaded
    }

    #[test]
    fn loaded_headers_may_differ_only_in_image_base() {
        let file = image(&[0xC3], &[]);
        let disk = PeImage::parse(&file).unwrap();
        let base = Va::new(0x7FF6_0000_0000);
        let loaded = loaded_headers(&file, base.get());
        assert_eq!(check_loaded_headers(&disk, &loaded, base), Ok(()));
        assert_eq!(
            check_loaded_headers(&disk, &loaded, Va::new(PREFERRED)),
            Err(IdentityError::ImageBase {
                header: base,
                actual: Va::new(PREFERRED)
            })
        );
        let mut tampered = loaded.clone();
        tampered[0x10] ^= 1;
        assert_eq!(
            check_loaded_headers(&disk, &tampered, base),
            Err(IdentityError::HeaderMismatch { offset: 0x10 })
        );
        assert_eq!(
            check_loaded_headers(&disk, &loaded[1..], base),
            Err(IdentityError::HeaderLength {
                expected: loaded.len(),
                found: loaded.len() - 1
            })
        );
    }

    fn relocations(entries: &[(u8, u16)]) -> Vec<Relocation> {
        let file = image(&[0xC3], &block(0x1000, entries));
        PeImage::parse(&file).unwrap().relocations().unwrap()
    }

    #[test]
    fn dir64_inside_code_is_rebased_and_absolute_is_skipped() {
        let preferred_target = PREFERRED + 0x1234;
        let actual = Va::new(0x7FF6_0000_0000);
        let mut code = vec![0x90; 32];
        code[8..16].copy_from_slice(&(preferred_target - PREFERRED + actual.get()).to_le_bytes());
        let mut expected = vec![0x90; 32];
        expected[8..16].copy_from_slice(&preferred_target.to_le_bytes());
        let relocations = relocations(&[(0, 0), (10, 8), (10, 0x100)]);
        normalise_relocations(&mut code, Rva::new(0x1000), &relocations, PREFERRED, actual)
            .unwrap();
        assert_eq!(code, expected);
    }

    #[test]
    fn refuses_straddling_and_unknown_relocations_inside_code() {
        let actual = Va::new(PREFERRED);
        for (entries, error) in [
            (
                vec![(10, 28)],
                IdentityError::StraddlingRelocation {
                    rva: Rva::new(0x101C),
                },
            ),
            (
                vec![(3, 4)],
                IdentityError::UnsupportedRelocation {
                    rva: Rva::new(0x1004),
                    kind: 3,
                },
            ),
        ] {
            let mut code = vec![0; 32];
            let relocations = relocations(&entries);
            assert_eq!(
                normalise_relocations(&mut code, Rva::new(0x1000), &relocations, PREFERRED, actual),
                Err(error)
            );
        }
        let before = [Relocation {
            rva: Rva::new(0x0FF8),
            kind: Relocation::DIR64,
        }];
        assert_eq!(
            normalise_relocations(&mut [0; 32], Rva::new(0x1000), &before, PREFERRED, actual),
            Ok(())
        );
        let straddle = [Relocation {
            rva: Rva::new(0x0FFC),
            kind: Relocation::DIR64,
        }];
        assert!(
            normalise_relocations(&mut [0; 32], Rva::new(0x1000), &straddle, PREFERRED, actual)
                .is_err()
        );
        let other_outside = [Relocation {
            rva: Rva::new(0x1020),
            kind: 3,
        }];
        assert_eq!(
            normalise_relocations(
                &mut [0; 32],
                Rva::new(0x1000),
                &other_outside,
                PREFERRED,
                actual
            ),
            Ok(())
        );
    }

    proptest! {
        #[test]
        fn normalisation_inverts_the_loader(
            values in prop::collection::vec(any::<u64>(), 1..8),
            actual in any::<u64>(),
        ) {
            let preferred: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
            let delta = actual.wrapping_sub(PREFERRED);
            let mut loaded: Vec<u8> = values.iter().flat_map(|v| v.wrapping_add(delta).to_le_bytes()).collect();
            let relocations: Vec<Relocation> = (0..values.len())
                .map(|i| Relocation { rva: Rva::new(0x1000 + u32::try_from(i * 8).unwrap()), kind: Relocation::DIR64 })
                .collect();
            normalise_relocations(&mut loaded, Rva::new(0x1000), &relocations, PREFERRED, Va::new(actual)).unwrap();
            prop_assert_eq!(loaded, preferred);
        }
    }
}
