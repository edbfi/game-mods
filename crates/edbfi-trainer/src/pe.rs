// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounds-checked parsing of 64-bit PE image files: headers, sections and base relocations.
//!
//! Only what identity verification needs is parsed. Every offset read from the file is checked before use, so
//! a truncated or hostile file yields a [`PeError`], never a panic.
#![forbid(unsafe_code)]

use crate::addr::Rva;
use crate::recovery::RecoveryAction;

const DOS_MAGIC: &[u8; 2] = b"MZ";
const NT_SIGNATURE: &[u8; 4] = b"PE\0\0";
const MACHINE_AMD64: u16 = 0x8664;
const PE32_PLUS_MAGIC: u16 = 0x20B;
const FILE_HEADER_LEN: usize = 20;
const SECTION_HEADER_LEN: usize = 40;
/// The PE format allows at most 96 sections.
const MAX_SECTIONS: u16 = 96;
/// Offsets inside the PE32+ optional header.
const OPT_IMAGE_BASE: usize = 24;
const OPT_SIZE_OF_IMAGE: usize = 56;
const OPT_RVA_COUNT: usize = 108;
const OPT_DATA_DIRECTORIES: usize = 112;
const BASE_RELOCATION_DIRECTORY: usize = 5;

/// One section header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
    name: [u8; 8],
    rva: Rva,
    virtual_size: u32,
    raw_offset: u32,
    raw_size: u32,
}

impl Section {
    /// The raw 8-byte, NUL-padded name.
    #[must_use]
    pub const fn name(&self) -> [u8; 8] {
        self.name
    }

    /// Where the section starts once loaded.
    #[must_use]
    pub const fn rva(&self) -> Rva {
        self.rva
    }

    /// Size once loaded.
    #[must_use]
    pub const fn virtual_size(&self) -> u32 {
        self.virtual_size
    }

    /// The section's bytes as the loader maps them: its raw data, then zeros up to its loaded size.
    #[cfg(any(test, feature = "simulate"))]
    pub(crate) fn loaded(&self, file: &[u8]) -> Option<Vec<u8>> {
        let raw = self.raw_size.min(self.virtual_size) as usize;
        let start = self.raw_offset as usize;
        let mut bytes = file.get(start..start.checked_add(raw)?)?.to_vec();
        bytes.resize(self.virtual_size as usize, 0);
        Some(bytes)
    }

    /// File bytes backing `[rva, rva + len)`, if this section's raw data covers all of it.
    fn file_range(&self, rva: Rva, len: u32) -> Option<std::ops::Range<usize>> {
        let start = rva.get().checked_sub(self.rva.get())?;
        let end = start.checked_add(len)?;
        let backed = if self.virtual_size == 0 {
            self.raw_size
        } else {
            self.raw_size.min(self.virtual_size)
        };
        if end > backed {
            return None;
        }
        let file_start = usize::try_from(self.raw_offset.checked_add(start)?).ok()?;
        Some(file_start..file_start.checked_add(usize::try_from(len).ok()?)?)
    }
}

/// A base relocation entry: the loader adjusts the value at `rva` when the image is not at its preferred base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Relocation {
    /// Address of the adjusted value.
    pub rva: Rva,
    /// The `IMAGE_REL_BASED_*` type from the entry's top four bits.
    pub kind: u8,
}

impl Relocation {
    /// `IMAGE_REL_BASED_ABSOLUTE`: padding, adjusts nothing.
    pub const ABSOLUTE: u8 = 0;
    /// `IMAGE_REL_BASED_DIR64`: a 64-bit absolute address.
    pub const DIR64: u8 = 10;
}

/// A parsed PE32+ AMD64 image file.
#[derive(Debug)]
pub struct PeImage<'a> {
    file: &'a [u8],
    nt_offset: usize,
    nt_len: usize,
    image_base: u64,
    size_of_image: u32,
    sections: Vec<Section>,
    relocation_directory: Option<(Rva, u32)>,
}

fn bytes<const N: usize>(file: &[u8], at: usize) -> Option<[u8; N]> {
    file.get(at..at.checked_add(N)?)?.try_into().ok()
}

fn u16_at(file: &[u8], at: usize) -> Option<u16> {
    bytes(file, at).map(u16::from_le_bytes)
}

fn u32_at(file: &[u8], at: usize) -> Option<u32> {
    bytes(file, at).map(u32::from_le_bytes)
}

fn u64_at(file: &[u8], at: usize) -> Option<u64> {
    bytes(file, at).map(u64::from_le_bytes)
}

impl<'a> PeImage<'a> {
    /// Parses the headers and section table of `file`.
    ///
    /// # Errors
    ///
    /// A [`PeError`] naming the first check that failed.
    pub fn parse(file: &'a [u8]) -> Result<Self, PeError> {
        if file.get(..2) != Some(DOS_MAGIC) {
            return Err(PeError::NotMz);
        }
        let lfanew = u32_at(file, 0x3C).ok_or(PeError::Truncated { what: "DOS header" })?;
        let nt_offset = usize::try_from(lfanew)
            .ok()
            .filter(|&offset| offset > 0)
            .ok_or(PeError::BadNtOffset)?;
        if file.get(nt_offset..).and_then(|nt| nt.get(..4)) != Some(NT_SIGNATURE) {
            return Err(PeError::NotPe);
        }
        let header = nt_offset + 4;
        let truncated = PeError::Truncated {
            what: "file header",
        };
        let machine = u16_at(file, header).ok_or(truncated)?;
        if machine != MACHINE_AMD64 {
            return Err(PeError::NotAmd64 { machine });
        }
        let section_count = u16_at(file, header + 2).ok_or(truncated)?;
        let optional_len = usize::from(u16_at(file, header + 16).ok_or(truncated)?);
        let optional = header + FILE_HEADER_LEN;
        let nt_len = 4 + FILE_HEADER_LEN + optional_len;
        if optional_len < OPT_DATA_DIRECTORIES || file.len() < nt_offset + nt_len {
            return Err(PeError::Truncated {
                what: "optional header",
            });
        }
        let magic = u16_at(file, optional).ok_or(truncated)?;
        if magic != PE32_PLUS_MAGIC {
            return Err(PeError::NotPe32Plus { magic });
        }
        let image_base = u64_at(file, optional + OPT_IMAGE_BASE).ok_or(truncated)?;
        let size_of_image = u32_at(file, optional + OPT_SIZE_OF_IMAGE).ok_or(truncated)?;
        let directory_count = u32_at(file, optional + OPT_RVA_COUNT).ok_or(truncated)?;
        let relocation_slot = OPT_DATA_DIRECTORIES + BASE_RELOCATION_DIRECTORY * 8;
        let relocation_directory = if usize::try_from(directory_count)
            .is_ok_and(|n| n > BASE_RELOCATION_DIRECTORY)
            && relocation_slot + 8 <= optional_len
        {
            let rva = u32_at(file, optional + relocation_slot).ok_or(truncated)?;
            let size = u32_at(file, optional + relocation_slot + 4).ok_or(truncated)?;
            (size != 0).then_some((Rva::new(rva), size))
        } else {
            None
        };

        if section_count > MAX_SECTIONS {
            return Err(PeError::TooManySections {
                count: section_count,
            });
        }
        let table = optional + optional_len;
        let sections = (0..usize::from(section_count))
            .map(|index| {
                let at = table + index * SECTION_HEADER_LEN;
                let field = |offset| {
                    u32_at(file, at + offset).ok_or(PeError::Truncated {
                        what: "section table",
                    })
                };
                Ok(Section {
                    name: bytes(file, at).ok_or(PeError::Truncated {
                        what: "section table",
                    })?,
                    virtual_size: field(8)?,
                    rva: Rva::new(field(12)?),
                    raw_size: field(16)?,
                    raw_offset: field(20)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            file,
            nt_offset,
            nt_len,
            image_base,
            size_of_image,
            sections,
            relocation_directory,
        })
    }

    /// The preferred load address.
    #[must_use]
    pub const fn image_base(&self) -> u64 {
        self.image_base
    }

    /// Bytes the loaded image spans.
    #[must_use]
    pub const fn size_of_image(&self) -> u32 {
        self.size_of_image
    }

    /// Every section header, in file order.
    #[must_use]
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// The first section called `name` (at most 8 bytes, NUL padding implied).
    #[must_use]
    pub fn section(&self, name: &str) -> Option<&Section> {
        let mut padded = [0; 8];
        padded
            .get_mut(..name.len())?
            .copy_from_slice(name.as_bytes());
        self.sections.iter().find(|section| section.name == padded)
    }

    /// The NT headers exactly as the file has them: signature, file header and optional header.
    #[must_use]
    pub fn nt_headers(&self) -> &'a [u8] {
        &self.file[self.nt_offset..self.nt_offset + self.nt_len]
    }

    /// Where the NT headers start in the file, and in the loaded image: `e_lfanew`.
    #[must_use]
    pub const fn nt_offset(&self) -> usize {
        self.nt_offset
    }

    /// Offset of `ImageBase` within [`nt_headers`](Self::nt_headers).
    pub const NT_IMAGE_BASE_OFFSET: usize = 4 + FILE_HEADER_LEN + OPT_IMAGE_BASE;

    /// Every base relocation entry, in file order.
    ///
    /// # Errors
    ///
    /// [`PeError::RelocationDirectory`] if the directory is not backed by one section's file data, or
    /// [`PeError::RelocationBlock`] for a malformed block.
    pub fn relocations(&self) -> Result<Vec<Relocation>, PeError> {
        let Some((rva, size)) = self.relocation_directory else {
            return Ok(Vec::new());
        };
        let range = self
            .sections
            .iter()
            .find_map(|section| section.file_range(rva, size))
            .ok_or(PeError::RelocationDirectory)?;
        let mut directory = self.file.get(range).ok_or(PeError::RelocationDirectory)?;
        let mut relocations = Vec::new();
        let mut offset = 0usize;
        while !directory.is_empty() {
            let bad = PeError::RelocationBlock { offset };
            let page = u32_at(directory, 0).ok_or(bad)?;
            let block_len = usize::try_from(u32_at(directory, 4).ok_or(bad)?).map_err(|_| bad)?;
            if block_len < 8 || block_len % 2 != 0 || block_len > directory.len() {
                return Err(bad);
            }
            for &entry in directory[8..block_len].as_chunks::<2>().0 {
                let entry = u16::from_le_bytes(entry);
                let kind = u8::try_from(entry >> 12).map_err(|_| bad)?;
                let target = Rva::new(page)
                    .checked_add(u32::from(entry & 0x0FFF))
                    .ok_or(bad)?;
                relocations.push(Relocation { rva: target, kind });
            }
            directory = &directory[block_len..];
            offset += block_len;
        }
        Ok(relocations)
    }
}

/// An image file that is not a well-formed PE32+ AMD64 executable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PeError {
    /// No `MZ` signature.
    #[error("not an executable: no MZ signature")]
    NotMz,
    /// `e_lfanew` is zero or does not fit.
    #[error("invalid NT header offset")]
    BadNtOffset,
    /// No `PE\0\0` signature at `e_lfanew`.
    #[error("not a PE image: no PE signature")]
    NotPe,
    /// The file ends inside a structure.
    #[error("truncated {what}")]
    Truncated {
        /// The structure being read.
        what: &'static str,
    },
    /// Built for another CPU.
    #[error("machine {machine:#06x} is not AMD64")]
    NotAmd64 {
        /// The file header's machine field.
        machine: u16,
    },
    /// A 32-bit (PE32) optional header.
    #[error("optional header magic {magic:#06x} is not PE32+")]
    NotPe32Plus {
        /// The optional header's magic field.
        magic: u16,
    },
    /// More sections than the format allows.
    #[error("{count} sections exceed the PE limit of 96")]
    TooManySections {
        /// The declared count.
        count: u16,
    },
    /// The relocation directory is not backed by a single section's file data.
    #[error("base relocation directory lies outside the file's sections")]
    RelocationDirectory,
    /// A relocation block has an impossible size or target.
    #[error("malformed base relocation block at directory offset {offset:#x}")]
    RelocationBlock {
        /// Byte offset of the block within the directory.
        offset: usize,
    },
}

impl PeError {
    /// The file on disk is not the supported game executable.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        RecoveryAction::UnsupportedBuild
    }
}

/// Builds synthetic PE files for tests.
#[cfg(test)]
pub(crate) mod testing {
    /// A minimal PE32+ AMD64 file with `.text` (at most 0x1000 bytes) at RVA 0x1000, file 0x400, and
    /// `.reloc` at RVA 0x2000 holding `blocks`.
    pub(crate) fn image(text: &[u8], blocks: &[u8]) -> Vec<u8> {
        assert!(text.len() <= 0x1000);
        let text_raw = text.len().next_multiple_of(0x200).max(0x400);
        let reloc_raw = 0x400 + text_raw;
        let mut file = vec![0u8; reloc_raw + blocks.len().max(1)];
        file[..2].copy_from_slice(b"MZ");
        file[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        let nt = 0x80;
        file[nt..nt + 4].copy_from_slice(b"PE\0\0");
        let header = nt + 4;
        file[header..header + 2].copy_from_slice(&0x8664u16.to_le_bytes());
        file[header + 2..header + 4].copy_from_slice(&2u16.to_le_bytes());
        file[header + 16..header + 18].copy_from_slice(&240u16.to_le_bytes());
        let optional = header + 20;
        file[optional..optional + 2].copy_from_slice(&0x20Bu16.to_le_bytes());
        file[optional + 24..optional + 32].copy_from_slice(&0x1_4000_0000u64.to_le_bytes());
        file[optional + 56..optional + 60].copy_from_slice(&0x3000u32.to_le_bytes());
        file[optional + 108..optional + 112].copy_from_slice(&16u32.to_le_bytes());
        let reloc_dir = optional + 112 + 5 * 8;
        file[reloc_dir..reloc_dir + 4].copy_from_slice(&0x2000u32.to_le_bytes());
        file[reloc_dir + 4..reloc_dir + 8]
            .copy_from_slice(&u32::try_from(blocks.len()).unwrap().to_le_bytes());
        let table = optional + 240;
        section(
            &mut file[table..table + 40],
            b".text\0\0\0",
            text.len(),
            0x1000,
            0x400,
            text_raw,
        );
        section(
            &mut file[table + 40..table + 80],
            b".reloc\0\0",
            blocks.len(),
            0x2000,
            u32::try_from(reloc_raw).unwrap(),
            blocks.len().max(1),
        );
        file[0x400..0x400 + text.len()].copy_from_slice(text);
        file[reloc_raw..reloc_raw + blocks.len()].copy_from_slice(blocks);
        file
    }

    fn section(
        header: &mut [u8],
        name: &[u8; 8],
        virtual_size: usize,
        rva: u32,
        raw: u32,
        raw_size: usize,
    ) {
        header[..8].copy_from_slice(name);
        header[8..12].copy_from_slice(&u32::try_from(virtual_size).unwrap().to_le_bytes());
        header[12..16].copy_from_slice(&rva.to_le_bytes());
        header[16..20].copy_from_slice(&u32::try_from(raw_size).unwrap().to_le_bytes());
        header[20..24].copy_from_slice(&raw.to_le_bytes());
    }

    /// One relocation block for `page` with the given `(type, offset)` entries.
    pub(crate) fn block(page: u32, entries: &[(u8, u16)]) -> Vec<u8> {
        let mut block = page.to_le_bytes().to_vec();
        block.extend_from_slice(&u32::try_from(8 + entries.len() * 2).unwrap().to_le_bytes());
        for &(kind, offset) in entries {
            block.extend_from_slice(&((u16::from(kind) << 12) | offset).to_le_bytes());
        }
        block
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{block, image};
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parses_a_well_formed_image() {
        let file = image(&[0xC3; 16], &block(0x1000, &[(10, 8), (0, 0)]));
        let pe = PeImage::parse(&file).unwrap();
        assert_eq!(pe.image_base(), 0x1_4000_0000);
        assert_eq!(pe.size_of_image(), 0x3000);
        let text = pe.section(".text").unwrap();
        assert_eq!((text.rva(), text.virtual_size()), (Rva::new(0x1000), 16));
        assert!(pe.section(".data").is_none() && pe.section("too-long-name").is_none());
        assert_eq!(pe.nt_headers().len(), 4 + 20 + 240);
        assert_eq!(
            pe.relocations().unwrap(),
            [
                Relocation {
                    rva: Rva::new(0x1008),
                    kind: 10
                },
                Relocation {
                    rva: Rva::new(0x1000),
                    kind: 0
                }
            ]
        );
    }

    #[test]
    fn rejects_each_header_check() {
        let good = image(&[0xC3], &[]);
        let mut cases: Vec<(Vec<u8>, PeError)> = Vec::new();
        let mut patched = |at: usize, value: &[u8], error| {
            let mut file = good.clone();
            file[at..at + value.len()].copy_from_slice(value);
            cases.push((file, error));
        };
        patched(0, b"ZM", PeError::NotMz);
        patched(0x3C, &0u32.to_le_bytes(), PeError::BadNtOffset);
        patched(0x80, b"PE\0\x01", PeError::NotPe);
        patched(
            0x84,
            &0x14Cu16.to_le_bytes(),
            PeError::NotAmd64 { machine: 0x14C },
        );
        patched(
            0x98,
            &0x10Bu16.to_le_bytes(),
            PeError::NotPe32Plus { magic: 0x10B },
        );
        patched(
            0x86,
            &97u16.to_le_bytes(),
            PeError::TooManySections { count: 97 },
        );
        patched(
            0x94,
            &100u16.to_le_bytes(),
            PeError::Truncated {
                what: "optional header",
            },
        );
        cases.push((
            good[..0x3E].to_vec(),
            PeError::Truncated { what: "DOS header" },
        ));
        cases.push((
            good[..0x100].to_vec(),
            PeError::Truncated {
                what: "optional header",
            },
        ));
        for (file, error) in cases {
            assert_eq!(PeImage::parse(&file).err(), Some(error));
        }
    }

    #[test]
    fn rejects_malformed_relocation_blocks() {
        let mut short = block(0x1000, &[]);
        short[4..8].copy_from_slice(&6u32.to_le_bytes());
        let mut odd = block(0x1000, &[(10, 0)]);
        odd[4..8].copy_from_slice(&9u32.to_le_bytes());
        let mut long = block(0x1000, &[(10, 0)]);
        long[4..8].copy_from_slice(&64u32.to_le_bytes());
        let overflow = block(u32::MAX, &[(10, 1)]);
        for blocks in [short, odd, long, overflow] {
            let file = image(&[0xC3], &blocks);
            assert_eq!(
                PeImage::parse(&file).unwrap().relocations(),
                Err(PeError::RelocationBlock { offset: 0 })
            );
        }
        let mut outside = image(&[0xC3], &block(0x1000, &[(10, 0)]));
        let reloc_dir = 0x84 + 20 + 112 + 5 * 8;
        outside[reloc_dir..reloc_dir + 4].copy_from_slice(&0x2FFCu32.to_le_bytes());
        assert_eq!(
            PeImage::parse(&outside).unwrap().relocations(),
            Err(PeError::RelocationDirectory)
        );
    }

    proptest! {
        #[test]
        fn never_panics_on_arbitrary_or_corrupted_input(
            noise in prop::collection::vec(any::<u8>(), 0..2048),
            flips in prop::collection::vec((0usize..0x900, any::<u8>()), 0..16),
        ) {
            if let Ok(pe) = PeImage::parse(&noise) {
                let _ = pe.relocations();
            }
            let mut file = image(&[0xC3; 32], &block(0x1000, &[(10, 0), (3, 4)]));
            for (at, value) in flips {
                if let Some(byte) = file.get_mut(at) {
                    *byte = value;
                }
            }
            if let Ok(pe) = PeImage::parse(&file) {
                let _ = (pe.nt_headers(), pe.relocations());
            }
        }
    }
}
