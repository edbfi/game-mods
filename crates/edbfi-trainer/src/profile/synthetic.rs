// SPDX-License-Identifier: AGPL-3.0-or-later
//! A small profile of a made-up game, for tests and examples. It describes no real executable.
//!
//! It has one of each shape: an ordinary gate, a call gate, a class toggle pair under an all-classes toggle,
//! a no-reload site and a derived pressure bit.
#![forbid(unsafe_code)]

use crate::addr::Rva;
use crate::chord::{Chord, Key, Modifiers};
use crate::control::{ControlBit, ControlSet};
use crate::identity::Sha256Digest;

use super::{
    BindingDefault, Checked, Class, Context, Derived, Game, GameOption, GateEffect, Group,
    HookKind, Identity, ModSlug, PlayerLayout, PressureKind, Profile, ReloadKind, Site, SiteId,
    TextSection, Toggle, class_toggles, concat,
};

const fn alt(key: Option<Key>) -> BindingDefault {
    match key {
        Some(key) => BindingDefault {
            since: 1,
            default: Some(Chord::new(Modifiers::ALT, key)),
        },
        None => panic!("unsupported key"),
    }
}

const fn cheat(
    key: &'static str,
    label: &'static str,
    controls: ControlSet,
    binding: BindingDefault,
    default: bool,
) -> Toggle {
    Toggle {
        key,
        label,
        hint: "A synthetic cheat.",
        group: "cheats",
        since: 1,
        controls,
        binding,
        default,
        gameplay: Checked::Pending,
    }
}

const CLASSES: [Class; 2] = [
    Class {
        key: "red",
        label: "Red",
        since: 1,
        bit: ControlBit::new(2),
        interned_ids: &[Rva::new(0x2000)],
    },
    Class {
        key: "blue",
        label: "Blue",
        since: 1,
        bit: ControlBit::new(3),
        interned_ids: &[Rva::new(0x2004), Rva::new(0x2008)],
    },
];

const CHEATS: [Toggle; 3] = [
    cheat(
        "alpha",
        "Alpha",
        ControlSet::of(ControlBit::new(0)),
        alt(Key::function(1)),
        false,
    ),
    cheat(
        "beta",
        "Beta",
        ControlSet::of(ControlBit::new(1)),
        alt(Key::function(2)),
        true,
    ),
    cheat(
        "all_colours",
        "All colours",
        ControlSet::span(ControlBit::new(2), ControlBit::new(3)),
        alt(Key::digit(1)),
        false,
    ),
];

const TOGGLES: [Toggle; 5] = concat(
    CHEATS,
    class_toggles(&CLASSES, "colours", "A synthetic class.", Checked::Pending),
);

const SITES: [Site; 4] = [
    Site {
        id: SiteId(1),
        rva: Rva::new(0x1100),
        original: crate::bytes!("48 89 5C 24 08"),
        context: Context {
            rva: Rva::new(0x1100),
            aob: crate::aob!("48 89 5C 24 08 57 48 83 EC 20 80 B9 F0 0B 00 00"),
        },
        kind: HookKind::Gate {
            bit: ControlBit::new(0),
            effect: GateEffect::ReturnTrue,
        },
    },
    Site {
        id: SiteId(2),
        rva: Rva::new(0x1200),
        original: crate::bytes!("E8 FB 00 00 00"),
        context: Context {
            rva: Rva::new(0x11F8),
            aob: crate::aob!("48 8B CB 0F 28 CE 90 90 E8 FB 00 00 00 0F 28 CE"),
        },
        kind: HookKind::Gate {
            bit: ControlBit::new(1),
            effect: GateEffect::SkipBatteryCall {
                callee: Rva::new(0x1300),
            },
        },
    },
    Site {
        id: SiteId(3),
        rva: Rva::new(0x1400),
        original: crate::bytes!("49 8B 06 41 2B FD"),
        context: Context {
            rva: Rva::new(0x1400),
            aob: crate::aob!("49 8B 06 41 2B FD 8B D7 49 8B CE FF 90 60 13 00 00"),
        },
        kind: HookKind::NoReload(ReloadKind::SharedR14),
    },
    Site {
        id: SiteId(4),
        rva: Rva::new(0x1500),
        original: crate::bytes!("40 53 48 83 EC 20"),
        context: Context {
            rva: Rva::new(0x1500),
            aob: crate::aob!("40 53 48 83 EC 20 48 8B D9 48 81 C1 ?? ?? 00 00"),
        },
        kind: HookKind::Pressure {
            bit: ControlBit::new(31),
            kind: PressureKind::RecoveryRcx,
        },
    },
];

/// The synthetic profile.
pub const SYNTHETIC: Profile = Profile {
    game: Game {
        name: "Synthetic Game",
        exe: "synthetic.exe",
    },
    slug: ModSlug {
        game: "synthetic-game",
        r#mod: "trainer",
    },
    identity: Identity {
        disk_sha256: Sha256Digest::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000001",
        ),
        text: TextSection {
            rva: Rva::new(0x1000),
            size: 0x1000,
            sha256: Sha256Digest::from_hex(
                "0000000000000000000000000000000000000000000000000000000000000002",
            ),
        },
    },
    player: PlayerLayout {
        manager: Rva::new(0x3000),
        player: 0x38,
        owner: 0xA0,
        weapon_owner: 0x118,
        weapon_config: 0x1B8,
        weapon_pressure: 0xC00,
        config_normal_pressure: 0x1320,
        pressure_class: "blue",
    },
    groups: &[
        Group {
            key: "cheats",
            title: "Cheats",
        },
        Group {
            key: "colours",
            title: "Colours",
        },
    ],
    toggles: &TOGGLES,
    options: &[GameOption {
        key: "keep",
        label: "Keep pressure",
        hint: "A synthetic option.",
        since: 1,
        default: true,
    }],
    derived: &[Derived {
        bit: ControlBit::new(31),
        option: "keep",
        when_any: &["alpha", "all_colours", "blue"],
    }],
    classes: &CLASSES,
    sites: &SITES,
    master_default: alt(Key::function(8)),
    settings_schema: 1,
};

const _: () = SYNTHETIC.check();
