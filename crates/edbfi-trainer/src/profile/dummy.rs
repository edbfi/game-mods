// SPDX-License-Identifier: AGPL-3.0-only
//! The profile of the `dummy_game` example, the stand-in game the Windows tests attach to.
//!
//! The dummy's code holds three sites: a predicate gated to return true on bit 0, a battery drain skipped on
//! bit 1, and a weapon fire whose round-count setter the no-reload hook keeps for the synthetic classes. Each
//! is found by the marker immediate beside it, so the profile follows whatever code the compiler emitted.
#![forbid(unsafe_code)]

use crate::addr::Rva;
use crate::aob::{Pattern, Token};
use crate::control::ControlBit;
use crate::identity::Sha256Digest;
use crate::pe::PeImage;

use super::synthetic::SYNTHETIC;
use super::{
    Class, Context, Game, GateEffect, HookKind, Identity, PlayerLayout, Profile, ReloadKind, Site,
    SiteId, TextSection,
};

/// The dummy's executable name.
pub const EXE: &str = "dummy_game.exe";

/// The profile of the dummy whose executable is `file` and whose first line of output was `layout`
/// (`layout manager=0x… red=0x… blue=0x… blue_alias=0x…`). Each call leaks the profile, which lives for
/// the rest of the process.
///
/// # Errors
///
/// What did not match: an unreadable executable or layout, or a missing site marker.
pub fn profile(file: &[u8], layout: &str) -> Result<&'static Profile, String> {
    let layout = Layout::parse(layout)?;
    let pe = PeImage::parse(file).map_err(|error| format!("the dummy is not an image: {error}"))?;
    let section = pe.section(".text").ok_or("the dummy has no code section")?;
    let text = section
        .loaded(file)
        .ok_or("the dummy's code section is cut off")?;
    let base = section.rva().get();
    let alpha = site(
        &text,
        base,
        &[
            0x48, 0x89, 0x5C, 0x24, 0x08, 0x57, 0x48, 0x83, 0xEC, 0x20, 0x31, 0xC0, 0xB9, 0x01,
            0x00, 0xED,
        ],
        (0, 5),
        1,
        HookKind::Gate {
            bit: ControlBit::new(0),
            effect: GateEffect::ReturnTrue,
        },
    )?;
    let drain_marker = [0xB9, 0x02, 0x00, 0xED, 0x5E, 0xE8];
    let at = find(&text, &drain_marker)?;
    let rel = text
        .get(at + 6..at + 10)
        .and_then(|bytes| bytes.try_into().ok())
        .map(i32::from_le_bytes)
        .ok_or("the drain's call is cut off")?;
    let callee = i64::from(base) + (at + 10) as i64 + i64::from(rel);
    let callee = Rva::new(u32::try_from(callee).map_err(|_| "the drain's callee is out of range")?);
    let drain = site(
        &text,
        base,
        &drain_marker,
        (5, 5),
        2,
        HookKind::Gate {
            bit: ControlBit::new(1),
            effect: GateEffect::SkipBatteryCall { callee },
        },
    )?;
    let fire = site(
        &text,
        base,
        &[
            0x8D, 0x50, 0xFF, 0xFF, 0x93, 0x60, 0x13, 0x00, 0x00, 0xB8, 0x03, 0x00, 0xED, 0x5E,
        ],
        (0, 9),
        3,
        HookKind::NoReload(ReloadKind::DirectCallbackRcx),
    )?;
    let classes = leak(vec![
        Class {
            interned_ids: leak(vec![layout.get("red")?]),
            ..SYNTHETIC.classes[0]
        },
        Class {
            interned_ids: leak(vec![layout.get("blue")?, layout.get("blue_alias")?]),
            ..SYNTHETIC.classes[1]
        },
    ]);
    Ok(Box::leak(Box::new(Profile {
        game: Game {
            name: "Dummy Game",
            exe: EXE,
        },
        identity: Identity {
            disk_sha256: Sha256Digest::of(file),
            text: TextSection {
                rva: section.rva(),
                size: section.virtual_size(),
                sha256: Sha256Digest::of(&text),
            },
        },
        player: PlayerLayout {
            manager: layout.get("manager")?,
            ..SYNTHETIC.player
        },
        classes,
        sites: leak(vec![alpha, drain, fire]),
        derived: &[],
        ..SYNTHETIC
    })))
}

/// The dummy's globals, by name.
struct Layout(Vec<(String, Rva)>);

impl Layout {
    fn parse(line: &str) -> Result<Self, String> {
        let pairs = line
            .trim()
            .strip_prefix("layout ")
            .ok_or_else(|| format!("not a layout line: {line:?}"))?;
        pairs
            .split(' ')
            .map(|pair| {
                let (key, value) = pair
                    .split_once('=')
                    .ok_or_else(|| format!("not a name=value pair: {pair:?}"))?;
                let value = u32::from_str_radix(value.trim_start_matches("0x"), 16)
                    .map_err(|error| format!("{key}: {error}"))?;
                Ok((key.to_owned(), Rva::new(value)))
            })
            .collect::<Result<_, String>>()
            .map(Self)
    }

    fn get(&self, key: &str) -> Result<Rva, String> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|&(_, rva)| rva)
            .ok_or_else(|| format!("the layout has no {key}"))
    }
}

fn leak<T>(items: Vec<T>) -> &'static [T] {
    Box::leak(items.into_boxed_slice())
}

fn find(text: &[u8], marker: &[u8]) -> Result<usize, String> {
    text.windows(marker.len())
        .position(|window| window == marker)
        .ok_or_else(|| format!("marker {marker:02X?} not in the dummy's code"))
}

/// A site at the first match of `marker` in `text`, `skip` bytes in and `len` long (`(skip, len)`), with the
/// 16 bytes from the marker as its signature.
fn site(
    text: &[u8],
    text_rva: u32,
    marker: &[u8],
    (skip, len): (usize, usize),
    id: u8,
    kind: HookKind,
) -> Result<Site, String> {
    let at = find(text, marker)?;
    let signature = text
        .get(at..at + 16)
        .ok_or("a signature runs past the dummy's code")?;
    let original = text
        .get(at + skip..at + skip + len)
        .ok_or("a site runs past the dummy's code")?;
    let rva = |offset: usize| {
        u32::try_from(offset)
            .ok()
            .and_then(|offset| text_rva.checked_add(offset))
            .map(Rva::new)
            .ok_or("a site is out of range")
    };
    Ok(Site {
        id: SiteId(id),
        rva: rva(at + skip)?,
        original: leak(original.to_vec()),
        context: Context {
            rva: rva(at)?,
            aob: Pattern::new(leak(
                signature.iter().map(|&byte| Token::Byte(byte)).collect(),
            ))
            .map_err(|error| format!("bad signature: {error:?}"))?,
        },
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_line_names_each_global() {
        let layout = Layout::parse("layout manager=0x3000 red=0x10 blue=0x20\n").unwrap();
        assert_eq!(layout.get("manager"), Ok(Rva::new(0x3000)));
        assert_eq!(layout.get("blue"), Ok(Rva::new(0x20)));
        assert_eq!(
            layout.get("green"),
            Err("the layout has no green".to_owned())
        );
        assert!(Layout::parse("rounds 29").is_err());
        assert!(Layout::parse("layout manager").is_err());
        assert!(Layout::parse("layout manager=0xZZ").is_err());
    }

    #[test]
    fn a_file_that_is_not_the_dummy_is_refused() {
        let line = "layout manager=0x3000 red=0x10 blue=0x20 blue_alias=0x24";
        assert!(profile(b"MZ", line).is_err());
    }
}
