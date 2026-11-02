// SPDX-License-Identifier: AGPL-3.0-only
//! Proving a running process is the supported build, without changing it.
//!
//! In order: the executable file's SHA-256 is the profile's; the loaded main module is that file, the same
//! size, with the same NT headers but for the actual `ImageBase`; the loaded code section, relocations
//! undone, hashes to the profile's fingerprint; and every site's signature matches exactly once, where the
//! profile says, with the original bytes in place. Until the game has unpacked its code the fingerprint
//! differs; [`IdentifyError::transient`] says so, and the caller may try again.
#![forbid(unsafe_code)]

use std::path::Path;

use crate::addr::{Rva, Va};
use crate::aob::MatchError;
use crate::identity::{self, IdentityError, Sha256Digest};
use crate::patch::OsError;
use crate::pe::{PeError, PeImage, Relocation};
use crate::profile::{Site, SiteId, ValidProfile};
use crate::recovery::RecoveryAction;

use super::Game;

/// The largest executable file read.
pub(crate) const EXECUTABLE_LIMIT: u64 = 1 << 30;

/// A file's base relocations and preferred base, kept to undo relocations later without reading the file.
#[derive(Clone, Debug)]
pub(crate) struct Relocations {
    entries: Vec<Relocation>,
    preferred: u64,
}

impl Relocations {
    /// Undoes the loader's relocations in `code`, the section at `rva` of an image loaded at `image`.
    pub(crate) fn undo(&self, code: &mut [u8], rva: Rva, image: Va) -> Result<(), IdentityError> {
        identity::normalise_relocations(code, rva, &self.entries, self.preferred, image)
    }
}

/// Checks `game` against `profile`, returning the image base and the file's relocations.
pub(super) fn identify(
    game: &dyn Game,
    profile: &ValidProfile<'static>,
) -> Result<(Va, Relocations), IdentifyError> {
    let profile = profile.profile();
    let path = game.image_path().map_err(IdentifyError::Os)?;
    let file = game
        .read_executable(&path, EXECUTABLE_LIMIT)
        .map_err(IdentifyError::Os)?;
    let disk = Sha256Digest::of(&file);
    if disk != profile.identity.disk_sha256 {
        return Err(IdentifyError::Unsupported { found: disk });
    }
    let pe = PeImage::parse(&file)?;
    let section = profile.identity.text;
    if !pe
        .section(".text")
        .is_some_and(|text| text.rva() == section.rva && text.virtual_size() == section.size)
    {
        return Err(IdentifyError::TextSection);
    }
    let module = game.main_module().map_err(IdentifyError::Os)?;
    if !same_path(&module.path, &path) {
        return Err(IdentifyError::ModulePath);
    }
    if module.size != pe.size_of_image() {
        return Err(IdentifyError::ModuleSize {
            expected: pe.size_of_image(),
            found: module.size,
        });
    }
    let image = module.base;
    let nt = pe.nt_headers();
    let mut loaded = vec![0; nt.len()];
    let headers = image
        .checked_add(pe.nt_offset() as u64)
        .map_err(|_| IdentifyError::ModulePath)?;
    game.read(headers, &mut loaded).map_err(IdentifyError::Os)?;
    identity::check_loaded_headers(&pe, &loaded, image)?;
    let relocations = Relocations {
        entries: pe.relocations()?,
        preferred: pe.image_base(),
    };
    let mut text = vec![0; section.size as usize];
    let at = image
        .at(section.rva)
        .map_err(|_| IdentifyError::TextSection)?;
    game.read(at, &mut text).map_err(IdentifyError::Os)?;
    relocations.undo(&mut text, section.rva, image)?;
    let sites: Vec<SiteCheck> = profile
        .sites
        .iter()
        .map(|site| SiteCheck {
            site: site.id,
            status: check_site(site, &text, section.rva),
        })
        .collect();
    if Sha256Digest::of(&text) != section.sha256 {
        return Err(IdentifyError::Code { sites });
    }
    if sites
        .iter()
        .any(|check| check.status != SiteStatus::Verified)
    {
        return Err(IdentifyError::Sites { sites });
    }
    Ok((image, relocations))
}

/// Whether two paths name the same file, ignoring case as Windows does.
fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase()
}

/// Checks one site against the code section `text`, which starts at `text_rva`.
pub(super) fn check_site(site: &Site, text: &[u8], text_rva: Rva) -> SiteStatus {
    let offset = |rva: Rva| rva.get().checked_sub(text_rva.get()).map(|at| at as usize);
    let (Some(context), Some(at)) = (offset(site.context.rva), offset(site.rva)) else {
        return SiteStatus::OutsideCode;
    };
    match site.context.aob.find_unique(text) {
        Err(MatchError::Missing) => return SiteStatus::ContextMissing,
        Err(MatchError::Ambiguous { .. }) => return SiteStatus::ContextAmbiguous,
        Ok(found) if found != context => {
            return SiteStatus::ContextMoved {
                found: Rva::new(text_rva.get().saturating_add(found as u32)),
            };
        }
        Ok(_) => {}
    }
    match text.get(at..at + site.original.len()) {
        Some(bytes) if bytes == site.original => SiteStatus::Verified,
        Some(_) => SiteStatus::OriginalDiffers,
        None => SiteStatus::OutsideCode,
    }
}

/// One site's read-only check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SiteCheck {
    /// The site.
    pub site: SiteId,
    /// What the check found.
    pub status: SiteStatus,
}

/// What a site's check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SiteStatus {
    /// The signature matches once, where the profile says, and the original bytes are in place.
    #[error("verified")]
    Verified,
    /// The signature matches nowhere.
    #[error("signature not found")]
    ContextMissing,
    /// The signature matches more than once.
    #[error("signature matches more than once")]
    ContextAmbiguous,
    /// The signature matches once, elsewhere.
    #[error("signature found at {found} instead")]
    ContextMoved {
        /// Where it matched.
        found: Rva,
    },
    /// The bytes at the site are not the original.
    #[error("the site's bytes differ from the original")]
    OriginalDiffers,
    /// The site lies outside the code section.
    #[error("outside the code section")]
    OutsideCode,
}

/// Why a process is not the supported build.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IdentifyError {
    /// The process or its file could not be inspected.
    #[error("cannot inspect the game: {0}")]
    Os(OsError),
    /// The executable is another build.
    #[error("unsupported game build (executable SHA-256 {found})")]
    Unsupported {
        /// The file's digest.
        found: Sha256Digest,
    },
    /// The executable is not a valid image.
    #[error(transparent)]
    Pe(#[from] PeError),
    /// The executable's code section is not where the profile says.
    #[error("the executable's code section differs from the profile")]
    TextSection,
    /// The loaded main module is not the executable the process reports.
    #[error("the loaded game is not the executable it was started from")]
    ModulePath,
    /// The loaded main module's size differs from the file's.
    #[error("the loaded game is {found:#x} bytes, its file says {expected:#x}")]
    ModuleSize {
        /// From the file.
        expected: u32,
        /// Loaded.
        found: u32,
    },
    /// The loaded image disagrees with its file.
    #[error(transparent)]
    Identity(#[from] IdentityError),
    /// The loaded code is not the supported build's, or not yet unpacked. Each site's check is attached.
    #[error("the game's code differs from the supported build")]
    Code {
        /// Every site's check.
        sites: Vec<SiteCheck>,
    },
    /// The code matches the supported build, but a site does not match the profile.
    #[error("a hook site does not match the profile")]
    Sites {
        /// Every site's check.
        sites: Vec<SiteCheck>,
    },
}

impl IdentifyError {
    /// Whether the game may still be unpacking its code, so a later attempt may succeed.
    #[must_use]
    pub const fn transient(&self) -> bool {
        match self {
            Self::Code { .. } => true,
            // A game that has only just started has no module list yet: `ERROR_BAD_LENGTH`,
            // `ERROR_PARTIAL_COPY` or `ERROR_NO_MORE_FILES` until its loader runs.
            Self::Os(error) => matches!(error.code, 18 | 24 | 299),
            _ => false,
        }
    }

    /// Every site's check, when the code section was read.
    #[must_use]
    pub fn sites(&self) -> Option<&[SiteCheck]> {
        match self {
            Self::Code { sites } | Self::Sites { sites } => Some(sites),
            _ => None,
        }
    }

    /// The one next step to offer.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        match self {
            Self::Os(_) => RecoveryAction::Retry,
            Self::Unsupported { .. } | Self::Pe(_) => RecoveryAction::UnsupportedBuild,
            Self::TextSection | Self::Sites { .. } => RecoveryAction::ReportBug,
            Self::ModulePath | Self::ModuleSize { .. } | Self::Code { .. } => {
                RecoveryAction::ModifiedGameCode
            }
            Self::Identity(error) => error.recovery(),
        }
    }
}
