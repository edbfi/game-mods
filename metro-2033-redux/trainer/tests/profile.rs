// SPDX-License-Identifier: AGPL-3.0-only
//! The Metro profile against the facts ledger, restated here independently of the profile's own tables.
#![expect(
    clippy::unwrap_used,
    reason = "helpers shared by tests; a missing key is a test failure"
)]

use edbfi_trainer::addr::Rva;
use edbfi_trainer::control::{ControlBit, Master};
use edbfi_trainer::identity::Sha256Digest;
use edbfi_trainer::profile::{
    Checked, Desired, GateEffect, HookKind, PressureKind, ReloadKind, ValidProfile,
};
use metro_2033_redux_trainer::profile::METRO;
use proptest::prelude::*;

fn valid() -> ValidProfile<'static> {
    ValidProfile::new(&METRO).unwrap()
}

/// Ordinary cheats in display order, with their control bit and default binding.
const CHEATS: [(&str, Option<u8>, &str); 8] = [
    ("unlimited_ammo", Some(0), "Alt+F1"),
    ("unlimited_throwables", Some(1), "Alt+F2"),
    ("unlimited_medkits", Some(2), "Alt+F3"),
    ("unlimited_filters", Some(3), "Alt+F6"),
    ("indestructible_mask", Some(4), "Alt+F5"),
    ("no_reload", None, "Alt+1"),
    ("god_mode", Some(5), "Alt+2"),
    ("unlimited_flashlight", Some(6), "Alt+3"),
];

/// F14: the classes in control-word order and their interned-id globals.
const CLASSES: [(&str, &[u32]); 24] = [
    ("ak74", &[0xf5d090]),
    ("aksu", &[0xf5d094]),
    ("rpk", &[0xf5d098]),
    ("ashot", &[0xf5d09c]),
    ("vsv", &[0xf5d0a0]),
    ("ventil", &[0xf5d0a4]),
    ("padonag", &[0xf5d0a8]),
    ("preved", &[0xf5d0ac]),
    ("vyhlop", &[0xf5d0b0]),
    ("saiga", &[0xf5d0b4]),
    ("tumak", &[0xf5d0b8]),
    ("2012", &[0xf5d0bc]),
    ("gatling", &[0xf5d0c0]),
    ("ubludok", &[0xf5d0c4]),
    ("uboynicheg", &[0xf5d0c8]),
    ("bigun", &[0xf5d0cc]),
    ("duplet", &[0xf5d0d0, 0xf5d20c, 0xf5d210, 0xf5d214]),
    ("revolver", &[0xf5d0d4]),
    ("abzac", &[0xf5d0d8]),
    ("hellbreath", &[0xf5d0dc]),
    ("hellshock", &[0xf5d0e0]),
    ("tihar", &[0xf5d0e4]),
    ("helsing", &[0xf5d0e8]),
    ("flamethrower", &[0xf5d0f8]),
];

#[test]
fn profile_is_valid() {
    assert_eq!(METRO.validate(), Ok(()));
    assert_eq!(valid().toggles().len(), 32);
}

#[test]
fn identity_matches_f1_f2() {
    assert_eq!(
        (METRO.game.name, METRO.game.exe),
        ("Metro 2033 Redux", "metro.exe")
    );
    assert_eq!(
        (METRO.slug.game, METRO.slug.r#mod),
        ("metro-2033-redux", "trainer")
    );
    assert_eq!(
        METRO.identity.disk_sha256.to_string(),
        "7c35095f27beab83b31914029eca0da07991bba8630ec12ba97eec43ffc60c38"
    );
    let text = METRO.identity.text;
    assert_eq!((text.rva, text.size), (Rva::new(0x1000), 10_614_332));
    assert_eq!(
        text.sha256.to_string(),
        "1f82b1598507d28b1dde205f8f41ec659b4d08b1ea5bcbdb36311941f1c73e93"
    );
}

#[test]
fn player_layout_matches_f12_f13() {
    let p = METRO.player;
    assert_eq!(p.manager, Rva::new(0xfbd820));
    assert_eq!((p.player, p.owner), (0x38, 0xa0));
    assert_eq!(
        (
            p.weapon_owner,
            p.weapon_config,
            p.weapon_pressure,
            p.config_normal_pressure
        ),
        (0x118, 0x1b8, 0xc00, 0x1320)
    );
    assert_eq!(p.pressure_class, "helsing");
}

#[test]
fn classes_match_f14_in_bit_order() {
    assert_eq!(METRO.classes.len(), CLASSES.len());
    for (index, (class, (key, ids))) in METRO.classes.iter().zip(CLASSES).enumerate() {
        assert_eq!(class.key, key);
        assert_eq!(usize::from(class.bit.index()), 7 + index, "{key}");
        let actual = class
            .interned_ids
            .iter()
            .map(|rva| rva.get())
            .collect::<Vec<_>>();
        assert_eq!(actual, ids, "{key}");
    }
}

#[test]
fn only_corroborated_weapon_names_are_display_names() {
    let renamed = [
        ("ak74", "Kalash"),
        ("uboynicheg", "Shambler"),
        ("revolver", "Revolver"),
        ("helsing", "Hellsing"),
    ];
    for class in METRO.classes {
        let expected = renamed
            .iter()
            .find(|(key, _)| *key == class.key)
            .map_or(class.key, |(_, label)| *label);
        assert_eq!(class.label, expected);
    }
}

#[test]
fn toggles_match_f17_and_the_default_bindings() {
    let valid = valid();
    let toggles = valid.toggles().collect::<Vec<_>>();
    for ((_, toggle), (key, bit, binding)) in toggles.iter().zip(CHEATS) {
        assert_eq!(toggle.key, key);
        let word = bit.map_or(0x7fff_ff80, |bit| 1u32 << bit);
        assert_eq!(toggle.controls.word(), word, "{key}");
        assert_eq!(
            toggle.binding.default.unwrap().to_string(),
            binding,
            "{key}"
        );
        assert_eq!(toggle.group, "cheats");
        let gameplay = if key == "unlimited_ammo" {
            Checked::Partial
        } else {
            Checked::Pending
        };
        assert_eq!(toggle.gameplay, gameplay, "{key}");
    }
    for ((_, toggle), (index, (key, _))) in toggles[8..].iter().zip(CLASSES.iter().enumerate()) {
        assert_eq!(toggle.key, *key);
        assert_eq!(toggle.controls.word(), 1 << (7 + index));
        assert!(toggle.binding.default.is_none(), "{key}");
        assert_eq!(
            (toggle.group, toggle.gameplay),
            ("weapons", Checked::Pending)
        );
    }
    assert_eq!(METRO.master_default.default.unwrap().to_string(), "Alt+F8");
    let all = toggles
        .iter()
        .fold(0, |word, (_, t)| word | t.controls.word());
    assert_eq!(all, 0x7fff_ffff, "bits 0-30 are selectable, bit 31 is not");
}

#[test]
fn pressure_is_an_option_and_derived_bit_31() {
    let [derived] = METRO.derived else {
        panic!("one derived bit");
    };
    assert_eq!(derived.bit, ControlBit::new(31));
    assert_eq!(derived.when_any, ["unlimited_ammo", "no_reload", "helsing"]);
    let [option] = METRO.options else {
        panic!("one option");
    };
    assert_eq!(
        (option.key, option.default),
        ("keep_hellsing_pressure", true)
    );
    assert_eq!(derived.option, option.key);
}

#[test]
fn defaults_select_nothing_and_keep_pressure() {
    let valid = valid();
    let desired = valid.defaults();
    assert_eq!(desired.master, Master::Active);
    assert!(desired.selection.is_empty());
    let keep = valid.option_id("keep_hellsing_pressure").unwrap();
    assert!(desired.options.contains(keep));
    assert_eq!(valid.effective(&desired).bits().word(), 0);
}

/// The sites as the ledger lists them: number, hook RVA, original length, context RVA and shape.
#[test]
fn sites_match_the_ledger() {
    let gate = |bit: u8, effect| HookKind::Gate {
        bit: ControlBit::new(bit),
        effect,
    };
    let pressure = |kind| HookKind::Pressure {
        bit: ControlBit::new(31),
        kind,
    };
    let throwable = GateEffect::Throwable {
        callee: Rva::new(0x24bda0),
        enabled: Rva::new(0x24bdd0),
    };
    let battery = GateEffect::SkipBatteryCall {
        callee: Rva::new(0x331730),
    };
    let expected = [
        (1, 0x2aecd0, 5, 0x2aecd0, gate(0, GateEffect::ReturnTrue)),
        (2, 0x2b9a55, 5, 0x2b9a52, gate(0, GateEffect::ReserveLoad)),
        (3, 0x2e870a, 5, 0x2e86ff, gate(1, throwable)),
        (4, 0x348b92, 5, 0x348b87, gate(1, throwable)),
        (5, 0x3292d1, 5, 0x3292d1, gate(3, GateEffect::ZeroXmm9)),
        (6, 0x32b6e9, 6, 0x32b6e9, gate(4, GateEffect::SkipMaskCall)),
        (7, 0x33079e, 7, 0x330795, gate(2, GateEffect::SkipMedkitAnd)),
        (
            8,
            0x1e3714,
            7,
            0x1e3710,
            gate(
                5,
                GateEffect::GodBranch {
                    target: Rva::new(0x1e372d),
                },
            ),
        ),
        (9, 0x1e982e, 5, 0x1e981b, gate(6, battery)),
        (10, 0x1e98f2, 5, 0x1e98e4, gate(6, battery)),
        (
            11,
            0x2a9a0c,
            6,
            0x2a9a0c,
            HookKind::NoReload(ReloadKind::SharedR14),
        ),
        (
            12,
            0x2c6ef4,
            9,
            0x2c6ef4,
            HookKind::NoReload(ReloadKind::DirectCallbackRcx),
        ),
        (
            13,
            0x2c6fab,
            7,
            0x2c6fab,
            HookKind::NoReload(ReloadKind::EventCallbackRcx),
        ),
        (
            14,
            0x2b9dce,
            6,
            0x2b9dce,
            HookKind::NoReload(ReloadKind::HellsingFireRdi),
        ),
        (
            15,
            0x2a9bb0,
            6,
            0x2a9bb0,
            pressure(PressureKind::RecoveryRcx),
        ),
        (
            16,
            0x2b6727,
            15,
            0x2b6727,
            pressure(PressureKind::Consumption1RbxXmm1),
        ),
        (
            17,
            0x2b68f4,
            12,
            0x2b68f4,
            pressure(PressureKind::Consumption2RbxXmm0),
        ),
    ];
    assert_eq!(METRO.sites.len(), expected.len());
    for (site, (id, rva, len, context, kind)) in METRO.sites.iter().zip(expected) {
        assert_eq!(site.id.0, id);
        assert_eq!(site.rva, Rva::new(rva), "{}", site.id);
        assert_eq!(site.original.len(), len, "{}", site.id);
        assert_eq!(site.context.rva, Rva::new(context), "{}", site.id);
        assert_eq!(site.kind, kind, "{}", site.id);
    }
    let returning = METRO
        .sites
        .iter()
        .filter(|site| matches!(site.kind, HookKind::NoReload(kind) if kind.returns_into_cave()))
        .map(|site| site.id.0)
        .collect::<Vec<_>>();
    assert_eq!(returning, [12], "only S12 is returned into");
}

/// F18, written from the ledger: the selection of each named toggle, by key.
fn oracle(desired: &Desired, valid: &ValidProfile<'_>) -> u32 {
    let on = |key: &str| desired.selection.contains(valid.toggle_id(key).unwrap());
    if desired.master == Master::Paused {
        return 0;
    }
    let mut word = 0;
    for (key, bit, _) in CHEATS {
        if let Some(bit) = bit {
            word |= u32::from(on(key)) << bit;
        }
    }
    for (index, (key, _)) in CLASSES.iter().enumerate() {
        word |= u32::from(on("no_reload") || on(key)) << (7 + index);
    }
    let keep = desired
        .options
        .contains(valid.option_id("keep_hellsing_pressure").unwrap());
    word | u32::from(keep && (on("unlimited_ammo") || on("no_reload") || on("helsing"))) << 31
}

fn desired(valid: &ValidProfile<'_>, selected: u32, keep: bool, master: Master) -> Desired {
    let mut desired = valid.defaults();
    desired.master = master;
    desired
        .options
        .set(valid.option_id("keep_hellsing_pressure").unwrap(), keep);
    for (id, _) in valid.toggles() {
        desired.selection.set(id, selected & 1 << id.index() != 0);
    }
    desired
}

#[test]
fn effective_mask_matches_f18_for_cheats_and_the_pressure_triggers() {
    let valid = valid();
    let helsing = valid.toggle_id("helsing").unwrap().index();
    let ak74 = valid.toggle_id("ak74").unwrap().index();
    for low in 0u32..1 << 8 {
        for extra in 0u32..4 {
            let selected = low | (extra & 1) << helsing | (extra >> 1) << ak74;
            for keep in [false, true] {
                for master in [Master::Active, Master::Paused] {
                    let desired = desired(&valid, selected, keep, master);
                    assert_eq!(
                        valid.effective(&desired).bits().word(),
                        oracle(&desired, &valid),
                        "{selected:#x} keep={keep} {master:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_shadowed_class_changes_nothing() {
    let valid = valid();
    let all = desired(&valid, 1 << 5, true, Master::Active);
    let mut both = all;
    both.selection.set(valid.toggle_id("ak74").unwrap(), true);
    assert_eq!(valid.effective(&all), valid.effective(&both));
    assert_eq!(valid.effective(&all).bits().word(), 0xffff_ff80);
}

proptest! {
    #[test]
    fn effective_mask_matches_f18(selected in any::<u32>(), keep in any::<bool>(), paused in any::<bool>()) {
        let valid = valid();
        let master = if paused { Master::Paused } else { Master::Active };
        let desired = desired(&valid, selected, keep, master);
        prop_assert_eq!(valid.effective(&desired).bits().word(), oracle(&desired, &valid));
    }
}

/// F9/F10 against the owner's capture of the unpacked `.text` (never committed): every context matches exactly
/// once, at its reviewed RVA. Run with `EDBFI_TEXT_CAPTURE=<file> cargo test -- --ignored`.
#[test]
#[ignore = "needs a local capture of the unpacked .text in EDBFI_TEXT_CAPTURE"]
fn contexts_match_once_in_a_text_capture() {
    let path =
        std::env::var_os("EDBFI_TEXT_CAPTURE").expect("EDBFI_TEXT_CAPTURE names the capture");
    let text = std::fs::read(path).unwrap();
    let section = METRO.identity.text;
    assert_eq!(text.len(), usize::try_from(section.size).unwrap());
    assert_eq!(Sha256Digest::of(&text), section.sha256);
    for site in METRO.sites {
        let at = site.context.aob.find_unique(&text).unwrap();
        let offset = site.context.rva.get() - section.rva.get();
        assert_eq!(at, usize::try_from(offset).unwrap(), "{}", site.id);
        if let HookKind::Gate {
            effect: GateEffect::GodBranch { target },
            ..
        } = site.kind
        {
            let at = usize::try_from(target.get() - section.rva.get()).unwrap();
            assert_eq!(
                text[at..at + 5],
                [0xB8, 1, 0, 0, 0],
                "{}: mov eax,1",
                site.id
            );
        }
    }
}
