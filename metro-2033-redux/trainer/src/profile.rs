// SPDX-License-Identifier: AGPL-3.0-only
//! Metro 2033 Redux, Steam build: every reverse-engineered fact the trainer relies on.
//!
//! Hook sites keep the S1–S17 numbers of the analysis documents in `docs/`. RVAs are relative to the loaded
//! `metro.exe`. Byte notation is the documents' own, so each line can be compared with its source.
#![forbid(unsafe_code)]

use edbfi_trainer::addr::Rva;
use edbfi_trainer::aob;
use edbfi_trainer::bytes;
use edbfi_trainer::chord::{Chord, Key, Modifiers};
use edbfi_trainer::control::{ControlBit, ControlSet};
use edbfi_trainer::identity::Sha256Digest;
use edbfi_trainer::profile::{
    BindingDefault, Checked, Class, Context, Derived, Game, GameOption, GateEffect, Group,
    HookKind, Identity, ModSlug, PlayerLayout, PressureKind, Profile, ReloadKind, Site, SiteId,
    TextSection, Toggle, class_toggles, concat,
};

/// The settings schema this profile writes.
const SCHEMA: u32 = 1;

const fn bit(index: u8) -> ControlBit {
    ControlBit::new(index)
}

const fn alt(key: Option<Key>) -> BindingDefault {
    match key {
        Some(key) => BindingDefault {
            since: SCHEMA,
            default: Some(Chord::new(Modifiers::ALT, key)),
        },
        None => panic!("unsupported key"),
    }
}

const fn class(key: &'static str, label: &'static str, index: u8, ids: &'static [Rva]) -> Class {
    Class {
        key,
        label,
        since: SCHEMA,
        bit: bit(7 + index),
        interned_ids: ids,
    }
}

/// The 24 weapon classes in control-word order: class `i` is bit `7 + i`. Each id is a global the game fills
/// by interning a `WEAPON_*` name; a weapon's configuration starts with the same DWORD. Labels are the game's
/// internal names except where a live capture corroborated the display name (Kalash, Shambler, Revolver) or
/// the analysis names the weapon (Hellsing).
pub const CLASSES: [Class; 24] = [
    class("ak74", "Kalash", 0, &[Rva::new(0xf5d090)]),
    class("aksu", "aksu", 1, &[Rva::new(0xf5d094)]),
    class("rpk", "rpk", 2, &[Rva::new(0xf5d098)]),
    class("ashot", "ashot", 3, &[Rva::new(0xf5d09c)]),
    class("vsv", "vsv", 4, &[Rva::new(0xf5d0a0)]),
    class("ventil", "ventil", 5, &[Rva::new(0xf5d0a4)]),
    class("padonag", "padonag", 6, &[Rva::new(0xf5d0a8)]),
    class("preved", "preved", 7, &[Rva::new(0xf5d0ac)]),
    class("vyhlop", "vyhlop", 8, &[Rva::new(0xf5d0b0)]),
    class("saiga", "saiga", 9, &[Rva::new(0xf5d0b4)]),
    class("tumak", "tumak", 10, &[Rva::new(0xf5d0b8)]),
    class("2012", "2012", 11, &[Rva::new(0xf5d0bc)]),
    class("gatling", "gatling", 12, &[Rva::new(0xf5d0c0)]),
    class("ubludok", "ubludok", 13, &[Rva::new(0xf5d0c4)]),
    class("uboynicheg", "Shambler", 14, &[Rva::new(0xf5d0c8)]),
    class("bigun", "bigun", 15, &[Rva::new(0xf5d0cc)]),
    // The 2B, 3B and 4B registrations are Duplet variants.
    class(
        "duplet",
        "duplet",
        16,
        &[
            Rva::new(0xf5d0d0),
            Rva::new(0xf5d20c),
            Rva::new(0xf5d210),
            Rva::new(0xf5d214),
        ],
    ),
    class("revolver", "Revolver", 17, &[Rva::new(0xf5d0d4)]),
    class("abzac", "abzac", 18, &[Rva::new(0xf5d0d8)]),
    class("hellbreath", "hellbreath", 19, &[Rva::new(0xf5d0dc)]),
    class("hellshock", "hellshock", 20, &[Rva::new(0xf5d0e0)]),
    class("tihar", "tihar", 21, &[Rva::new(0xf5d0e4)]),
    class("helsing", "Hellsing", 22, &[Rva::new(0xf5d0e8)]),
    class("flamethrower", "flamethrower", 23, &[Rva::new(0xf5d0f8)]),
];

const fn cheat(
    key: &'static str,
    label: &'static str,
    hint: &'static str,
    controls: ControlSet,
    binding: BindingDefault,
    gameplay: Checked,
) -> Toggle {
    Toggle {
        key,
        label,
        hint,
        group: "cheats",
        since: SCHEMA,
        controls,
        binding,
        default: false,
        gameplay,
    }
}

/// Display order; the bits are the control word's (ammo 0, throwables 1, medkits 2, filters 3, mask 4, god
/// mode 5, flashlight 6). Alt+F4 would close the game, so filters move to Alt+F6.
const CHEATS: [Toggle; 8] = [
    cheat(
        "unlimited_ammo",
        "Unlimited ammo",
        "Keeps reserve ammo; you still reload.",
        ControlSet::of(bit(0)),
        alt(Key::function(1)),
        Checked::Partial,
    ),
    cheat(
        "unlimited_throwables",
        "Unlimited throwables",
        "Keeps throwing knives, hand grenades and incendiaries.",
        ControlSet::of(bit(1)),
        alt(Key::function(2)),
        Checked::Pending,
    ),
    cheat(
        "unlimited_medkits",
        "Unlimited medkits",
        "Keeps medkits when you heal.",
        ControlSet::of(bit(2)),
        alt(Key::function(3)),
        Checked::Pending,
    ),
    cheat(
        "unlimited_filters",
        "Unlimited filters",
        "Stops gas mask filter time running down.",
        ControlSet::of(bit(3)),
        alt(Key::function(6)),
        Checked::Pending,
    ),
    cheat(
        "indestructible_mask",
        "Indestructible mask",
        "The gas mask you wear takes no damage.",
        ControlSet::of(bit(4)),
        alt(Key::function(5)),
        Checked::Pending,
    ),
    cheat(
        "no_reload",
        "No reload, all weapons",
        "Keeps loaded rounds for every weapon class.",
        ControlSet::span(bit(7), bit(30)),
        alt(Key::digit(1)),
        Checked::Pending,
    ),
    cheat(
        "god_mode",
        "God mode",
        "Turns on the game's own player invulnerability.",
        ControlSet::of(bit(5)),
        alt(Key::digit(2)),
        Checked::Pending,
    ),
    cheat(
        "unlimited_flashlight",
        "Unlimited flashlight",
        "Stops flashlight and night vision battery drain.",
        ControlSet::of(bit(6)),
        alt(Key::digit(3)),
        Checked::Pending,
    ),
];

const TOGGLES: [Toggle; 32] = concat(
    CHEATS,
    class_toggles(
        &CLASSES,
        "weapons",
        "Keeps this weapon's loaded rounds when you fire.",
        Checked::Pending,
    ),
);

/// The hook sites. Every gate prologue reads the control word once and tests its bit.
const SITES: [Site; 17] = [
    // Unlimited ammo: the reserve-consumption predicate's entry returns true (ammo-analysis.md).
    Site {
        id: SiteId(1),
        rva: Rva::new(0x2aecd0),
        original: bytes!("48 89 5C 24 08"),
        context: Context {
            rva: Rva::new(0x2aecd0),
            aob: aob!("48 89 5C 24 08 57 48 83 EC 20 80 B9 F0 0B 00 00 00 48 8B D9"),
        },
        kind: HookKind::Gate {
            bit: bit(0),
            effect: GateEffect::ReturnTrue,
        },
    },
    // Unlimited ammo, Hellsing reserve: extended back from `0x2b9a58 2B DF` to keep the load.
    Site {
        id: SiteId(2),
        rva: Rva::new(0x2b9a55),
        original: bytes!("49 8B 06 2B DF"),
        context: Context {
            rva: Rva::new(0x2b9a52),
            aob: aob!("41 FF D1 49 8B 06 2B DF 8B D3 49 8B CE FF 90 F8 01 00 00"),
        },
        kind: HookKind::Gate {
            bit: bit(0),
            effect: GateEffect::ReserveLoad,
        },
    },
    // Unlimited throwables: the action at `0x24bda0 + 0x30` skips its consumption wrapper
    // (consumables-analysis.md).
    Site {
        id: SiteId(3),
        rva: Rva::new(0x2e870a),
        original: bytes!("E8 91 36 F6 FF"),
        context: Context {
            rva: Rva::new(0x2e86ff),
            aob: aob!("48 8d 96 18 01 00 00 4c 89 70 e8 e8 91 36 f6 ff 48 8b 8e 18 01"),
        },
        kind: HookKind::Gate {
            bit: bit(1),
            effect: GateEffect::Throwable {
                callee: Rva::new(0x24bda0),
                enabled: Rva::new(0x24bdd0),
            },
        },
    },
    Site {
        id: SiteId(4),
        rva: Rva::new(0x348b92),
        original: bytes!("E8 09 32 F0 FF"),
        context: Context {
            rva: Rva::new(0x348b87),
            aob: aob!("c1 60 0a 00 00 41 8b f8 4c 8b e2 e8 09 32 f0 ff 49 8d b6 b0 05"),
        },
        kind: HookKind::Gate {
            bit: bit(1),
            effect: GateEffect::Throwable {
                callee: Rva::new(0x24bda0),
                enabled: Rva::new(0x24bdd0),
            },
        },
    },
    // Unlimited filters: `mulss xmm9,xmm2` becomes a zero, which feeds the timer and its observers
    // (gasmask-analysis.md).
    Site {
        id: SiteId(5),
        rva: Rva::new(0x3292d1),
        original: bytes!("F3 44 0F 59 CA"),
        context: Context {
            rva: Rva::new(0x3292d1),
            aob: aob!("F3 44 0F 59 CA 44 0F 28 C0 F3 44 0F 5E 86 54 04 00 00 F3 41 0F 5C C1"),
        },
        kind: HookKind::Gate {
            bit: bit(3),
            effect: GateEffect::ZeroXmm9,
        },
    },
    // Indestructible mask: skips the damage dispatch `call [rax+0x1b0]` (gasmask-analysis.md).
    Site {
        id: SiteId(6),
        rva: Rva::new(0x32b6e9),
        original: bytes!("FF 90 B0 01 00 00"),
        context: Context {
            rva: Rva::new(0x32b6e9),
            aob: aob!("FF 90 B0 01 00 00 0F 57 C0 0F 2F 83 08 03 00 00 72 48"),
        },
        kind: HookKind::Gate {
            bit: bit(4),
            effect: GateEffect::SkipMaskCall,
        },
    },
    // Unlimited medkits: skips `and [r8+0x478],eax`, which clears the used medkit (consumables-analysis.md).
    Site {
        id: SiteId(7),
        rva: Rva::new(0x33079e),
        original: bytes!("41 21 80 78 04 00 00"),
        context: Context {
            rva: Rva::new(0x330795),
            aob: aob!(
                "b8 01 00 00 00 d3 e0 f7 d0 41 21 80 78 04 00 00 41 c7 80 84 04 00 00 ff 00 00 00"
            ),
        },
        kind: HookKind::Gate {
            bit: bit(2),
            effect: GateEffect::SkipMedkitAnd,
        },
    },
    // God mode: `cmp byte [rcx+0x1344],2`, extended back from `0x1e371b 75 1A`; selected jumps to the
    // existing `mov eax,1` return (survival-analysis.md).
    Site {
        id: SiteId(8),
        rva: Rva::new(0x1e3714),
        original: bytes!("80 B9 44 13 00 00 02"),
        context: Context {
            rva: Rva::new(0x1e3710),
            aob: aob!("48 83 EC 28 80 B9 44 13 00 00 02 75 1A 85 D2 74 0C 48 8D 0D F8 F2 93 00"),
        },
        kind: HookKind::Gate {
            bit: bit(5),
            effect: GateEffect::GodBranch {
                target: Rva::new(0x1e372d),
            },
        },
    },
    // Unlimited flashlight: skips the battery drain call (survival-analysis.md).
    Site {
        id: SiteId(9),
        rva: Rva::new(0x1e982e),
        original: bytes!("E8 FD 7E 14 00"),
        context: Context {
            rva: Rva::new(0x1e981b),
            aob: aob!(
                "48 8B CB F3 0F 59 48 78 0F 5B D2 F3 0F 59 15 42 F6 C7 00 E8 FD 7E 14 00 0F 28 CE"
            ),
        },
        kind: HookKind::Gate {
            bit: bit(6),
            effect: GateEffect::SkipBatteryCall {
                callee: Rva::new(0x331730),
            },
        },
    },
    // Unlimited flashlight, night vision.
    Site {
        id: SiteId(10),
        rva: Rva::new(0x1e98f2),
        original: bytes!("E8 39 7E 14 00"),
        context: Context {
            rva: Rva::new(0x1e98e4),
            aob: aob!("F3 41 0F 59 F9 4C 8B CE 48 8B CB 0F 28 CF E8 39 7E 14 00 48 8B 07 0F 28 CE"),
        },
        kind: HookKind::Gate {
            bit: bit(6),
            effect: GateEffect::SkipBatteryCall {
                callee: Rva::new(0x331730),
            },
        },
    },
    // No reload: `mov rax,[r14]; sub edi,r13d` in the shared fire routine (no-reload-analysis.md).
    Site {
        id: SiteId(11),
        rva: Rva::new(0x2a9a0c),
        original: bytes!("49 8B 06 41 2B FD"),
        context: Context {
            rva: Rva::new(0x2a9a0c),
            aob: aob!("49 8B 06 41 2B FD 8B D7 49 8B CE FF 90 60 13 00 00"),
        },
        kind: HookKind::NoReload(ReloadKind::SharedR14),
    },
    // `lea edx,[rax-1]; call [rbx+0x1360]`: the setter call returns into the hook's code.
    Site {
        id: SiteId(12),
        rva: Rva::new(0x2c6ef4),
        original: bytes!("8D 50 FF FF 93 60 13 00 00"),
        context: Context {
            rva: Rva::new(0x2c6ef4),
            aob: aob!("8D 50 FF FF 93 60 13 00 00 69 97 20 0C 00 00 E8 03 00 00"),
        },
        kind: HookKind::NoReload(ReloadKind::DirectCallbackRcx),
    },
    // `lea edx,[rax-1]; mov rax,[rsi-8]`: rejoins before the game's own call.
    Site {
        id: SiteId(13),
        rva: Rva::new(0x2c6fab),
        original: bytes!("8D 50 FF 48 8B 46 F8"),
        context: Context {
            rva: Rva::new(0x2c6fab),
            aob: aob!("8D 50 FF 48 8B 46 F8 FF 90 60 13 00 00 69 96 F8 0B 00 00 E8 03 00 00"),
        },
        kind: HookKind::NoReload(ReloadKind::EventCallbackRcx),
    },
    // `mov rcx,rdi; lea edx,[rax-1]` in the Hellsing's fire routine.
    Site {
        id: SiteId(14),
        rva: Rva::new(0x2b9dce),
        original: bytes!("48 8B CF 8D 50 FF"),
        context: Context {
            rva: Rva::new(0x2b9dce),
            aob: aob!("48 8B CF 8D 50 FF FF 93 60 13 00 00 C6 87 10 0C 00 00 01"),
        },
        kind: HookKind::NoReload(ReloadKind::HellsingFireRdi),
    },
    // Hellsing pressure: the recovery function's entry.
    Site {
        id: SiteId(15),
        rva: Rva::new(0x2a9bb0),
        original: bytes!("40 53 48 83 EC 20"),
        context: Context {
            rva: Rva::new(0x2a9bb0),
            aob: aob!(
                "40 53 48 83 ec 20 48 8b d9 48 81 c1 30 04 00 00 48 8b 01 ff 90 c8 01 00 00 84 c0 75 12 48 8b 03"
            ),
        },
        kind: HookKind::Pressure {
            bit: bit(31),
            kind: PressureKind::RecoveryRcx,
        },
    },
    Site {
        id: SiteId(16),
        rva: Rva::new(0x2b6727),
        original: bytes!("F3 0F 10 8B 00 0C 00 00 0F 57 C0 F3 0F 5C CE"),
        context: Context {
            rva: Rva::new(0x2b6727),
            aob: aob!(
                "f3 0f 10 8b 00 0c 00 00 0f 57 c0 f3 0f 5c ce 0f 2f c8 f3 0f 11 8b 00 0c 00 00 73 1a c7 83 00 0c"
            ),
        },
        kind: HookKind::Pressure {
            bit: bit(31),
            kind: PressureKind::Consumption1RbxXmm1,
        },
    },
    Site {
        id: SiteId(17),
        rva: Rva::new(0x2b68f4),
        original: bytes!("F3 0F 10 83 00 0C 00 00 F3 0F 5C C7"),
        context: Context {
            rva: Rva::new(0x2b68f4),
            aob: aob!(
                "f3 0f 10 83 00 0c 00 00 f3 0f 5c c7 0f 2f c6 f3 0f 11 83 00 0c 00 00 73 16 48 8b 03 48 8b cb c7"
            ),
        },
        kind: HookKind::Pressure {
            bit: bit(31),
            kind: PressureKind::Consumption2RbxXmm0,
        },
    },
];

/// Metro 2033 Redux, Steam build.
pub const METRO: Profile = Profile {
    game: Game {
        name: "Metro 2033 Redux",
        exe: "metro.exe",
    },
    slug: ModSlug {
        game: "metro-2033-redux",
        r#mod: "trainer",
    },
    identity: Identity {
        disk_sha256: Sha256Digest::from_hex(
            "7c35095f27beab83b31914029eca0da07991bba8630ec12ba97eec43ffc60c38",
        ),
        // Packed on disk: the code is unpacked at startup and hashed once relocations are undone.
        text: TextSection {
            rva: Rva::new(0x1000),
            size: 0xa1f63c,
            sha256: Sha256Digest::from_hex(
                "1f82b1598507d28b1dde205f8f41ec659b4d08b1ea5bcbdb36311941f1c73e93",
            ),
        },
    },
    // no-reload-analysis.md: player ownership and weapon selection.
    player: PlayerLayout {
        manager: Rva::new(0xfbd820),
        player: 0x38,
        owner: 0xa0,
        weapon_owner: 0x118,
        weapon_config: 0x1b8,
        weapon_pressure: 0xc00,
        config_normal_pressure: 0x1320,
        pressure_class: "helsing",
    },
    groups: &[
        Group {
            key: "cheats",
            title: "Cheats",
        },
        Group {
            key: "weapons",
            title: "No reload by weapon",
        },
    ],
    toggles: &TOGGLES,
    options: &[GameOption {
        key: "keep_hellsing_pressure",
        label: "Keep Hellsing pressure",
        hint: "Holds normal Hellsing pressure with unlimited ammo or no reload.",
        since: SCHEMA,
        default: true,
    }],
    derived: &[Derived {
        bit: bit(31),
        option: "keep_hellsing_pressure",
        when_any: &["unlimited_ammo", "no_reload", "helsing"],
    }],
    classes: &CLASSES,
    sites: &SITES,
    master_default: alt(Key::function(8)),
    settings_schema: SCHEMA,
};

const _: () = METRO.check();
