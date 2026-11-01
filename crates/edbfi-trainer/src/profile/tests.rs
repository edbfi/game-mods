// SPDX-License-Identifier: AGPL-3.0-only
use super::synthetic::SYNTHETIC;
use super::*;
use crate::addr::Rva;
use crate::chord::{Key, Modifiers, Refusal};
use crate::control::{ControlBit, Master};

fn leak<T>(items: Vec<T>) -> &'static [T] {
    items.leak()
}

/// `SYNTHETIC` after `edit`, validated.
fn validate(edit: impl FnOnce(&mut Profile)) -> Result<(), ProfileError> {
    let mut profile = SYNTHETIC;
    edit(&mut profile);
    profile.validate()
}

fn toggles(edit: impl FnOnce(&mut Vec<Toggle>)) -> impl FnOnce(&mut Profile) {
    move |profile| {
        let mut toggles = profile.toggles.to_vec();
        edit(&mut toggles);
        profile.toggles = leak(toggles);
    }
}

fn sites(edit: impl FnOnce(&mut Vec<Site>)) -> impl FnOnce(&mut Profile) {
    move |profile| {
        let mut sites = profile.sites.to_vec();
        edit(&mut sites);
        profile.sites = leak(sites);
    }
}

fn classes(edit: impl FnOnce(&mut Vec<Class>)) -> impl FnOnce(&mut Profile) {
    move |profile| {
        let mut classes = profile.classes.to_vec();
        edit(&mut classes);
        profile.classes = leak(classes);
    }
}

fn bit(index: u8) -> ControlBit {
    ControlBit::new(index)
}

fn alt(key: Option<Key>) -> Option<Chord> {
    key.map(|key| Chord::new(Modifiers::ALT, key))
}

#[test]
fn synthetic_profile_is_valid() {
    assert_eq!(SYNTHETIC.validate(), Ok(()));
    let valid = ValidProfile::new(&SYNTHETIC).unwrap();
    assert_eq!(valid.toggles().len(), 5);
    assert_eq!(SiteId(12).to_string(), "S12");
}

#[test]
fn game_slug_schema_and_text_are_checked() {
    assert_eq!(validate(|p| p.game.name = ""), Err(ProfileError::Game));
    assert_eq!(
        validate(|p| p.game.exe = "bin/x.exe"),
        Err(ProfileError::Game)
    );
    for part in ["", "Game", "a--b", "-a", "a-", "a_b"] {
        assert_eq!(
            validate(|p| p.slug.game = part),
            Err(ProfileError::Slug { part }),
            "{part:?}"
        );
    }
    assert_eq!(
        validate(|p| p.slug.r#mod = "Trainer"),
        Err(ProfileError::Slug { part: "Trainer" })
    );
    assert_eq!(
        validate(|p| p.settings_schema = 0),
        Err(ProfileError::Schema)
    );
    assert_eq!(
        validate(|p| p.identity.text.size = 0),
        Err(ProfileError::TextSection)
    );
    assert_eq!(
        validate(|p| p.identity.text.rva = Rva::new(u32::MAX)),
        Err(ProfileError::TextSection)
    );
}

#[test]
fn keys_are_lowercase_unique_and_unreserved() {
    for key in ["", "Alpha", "_a", "a-b", "a b"] {
        assert_eq!(
            validate(toggles(|t| t[0].key = key)),
            Err(ProfileError::KeySyntax { key }),
            "{key:?}"
        );
    }
    assert_eq!(validate(toggles(|t| t[1].key = "2012_x")), Ok(()));
    assert_eq!(
        validate(toggles(|t| t[0].key = "master")),
        Err(ProfileError::ReservedKey { key: "master" })
    );
    assert_eq!(
        validate(toggles(|t| t[1].key = "alpha")),
        Err(ProfileError::DuplicateKey { key: "alpha" })
    );
    assert_eq!(
        validate(|p| p.options = leak(vec![GameOption {
            key: "beta",
            ..p.options[0]
        }])),
        Err(ProfileError::DuplicateKey { key: "beta" })
    );
    assert_eq!(
        validate(|p| p.options = leak(vec![GameOption {
            key: "sounds",
            ..p.options[0]
        }])),
        Err(ProfileError::ReservedKey { key: "sounds" })
    );
    assert_eq!(
        validate(|p| p.groups = leak(vec![
            Group {
                key: "bindings",
                ..p.groups[0]
            },
            p.groups[1]
        ])),
        Err(ProfileError::ReservedKey { key: "bindings" })
    );
    assert_eq!(
        validate(|p| p.groups = leak(vec![p.groups[0], p.groups[1], p.groups[0]])),
        Err(ProfileError::DuplicateKey { key: "cheats" })
    );
}

#[test]
fn text_is_bounded_printable_ascii() {
    for label in [
        "",
        " Alpha",
        "Alpha ",
        "Ünlimited",
        "a\u{1b}[2J",
        &"x".repeat(25),
    ] {
        let label: &'static str = String::from(label).leak();
        assert_eq!(
            validate(toggles(|t| t[0].label = label)),
            Err(ProfileError::Text { owner: "alpha" }),
            "{label:?}"
        );
    }
    assert_eq!(
        validate(toggles(|t| t[0].label = "x".repeat(24).leak())),
        Ok(())
    );
    assert_eq!(
        validate(toggles(|t| t[0].hint = "x".repeat(73).leak())),
        Err(ProfileError::Text { owner: "alpha" })
    );
    assert_eq!(
        validate(classes(|c| c[0].label = "")),
        Err(ProfileError::Text { owner: "red" })
    );
}

#[test]
fn groups_and_limits() {
    assert_eq!(
        validate(toggles(|t| t[0].group = "nowhere")),
        Err(ProfileError::UnknownGroup {
            toggle: "alpha",
            group: "nowhere"
        })
    );
    assert_eq!(
        validate(|p| p.groups = leak(vec![
            p.groups[0],
            p.groups[1],
            Group {
                key: "spare",
                title: "Spare"
            }
        ])),
        Err(ProfileError::EmptyGroup { group: "spare" })
    );
    let mut many = SYNTHETIC.toggles.to_vec();
    many.resize(65, SYNTHETIC.toggles[0]);
    assert_eq!(
        validate(|p| p.toggles = leak(many)),
        Err(ProfileError::TooMany)
    );
}

#[test]
fn since_lies_in_the_schema() {
    assert_eq!(
        validate(toggles(|t| t[0].since = 0)),
        Err(ProfileError::Since { key: "alpha" })
    );
    assert_eq!(
        validate(toggles(|t| t[0].binding.since = 2)),
        Err(ProfileError::Since { key: "alpha" })
    );
    assert_eq!(
        validate(|p| p.master_default.since = 2),
        Err(ProfileError::Since { key: "master" })
    );
    assert_eq!(
        validate(classes(|c| c[1].since = 2)),
        Err(ProfileError::Since { key: "blue" })
    );
    let mut options = SYNTHETIC.options.to_vec();
    options[0].since = 0;
    assert_eq!(
        validate(|p| p.options = leak(options)),
        Err(ProfileError::Since { key: "keep" })
    );
    assert_eq!(
        validate(|p| {
            p.settings_schema = 2;
            p.toggles = leak(
                p.toggles
                    .iter()
                    .map(|t| Toggle { since: 2, ..*t })
                    .collect(),
            );
        }),
        Err(ProfileError::ClassToggle { class: "red" }),
        "a class toggle keeps its class's since"
    );
}

#[test]
fn default_bindings_are_allowed_and_unique() {
    assert_eq!(
        validate(toggles(|t| t[0].binding.default = alt(Key::function(4)))),
        Err(ProfileError::RefusedBinding {
            key: "alpha",
            refusal: Refusal::System
        })
    );
    assert_eq!(
        validate(|p| p.master_default.default =
            Key::letter(b'M').map(|k| Chord::new(Modifiers::NONE, k))),
        Err(ProfileError::RefusedBinding {
            key: "master",
            refusal: Refusal::Typing
        })
    );
    assert_eq!(
        validate(toggles(|t| t[1].binding.default = t[0].binding.default)),
        Err(ProfileError::DuplicateBinding {
            first: "alpha",
            second: "beta"
        })
    );
    assert_eq!(
        validate(toggles(|t| t[2].binding.default = alt(Key::function(8)))),
        Err(ProfileError::DuplicateBinding {
            first: "master",
            second: "all_colours"
        })
    );
    assert_eq!(validate(toggles(|t| t[0].binding.default = None)), Ok(()));
    assert_eq!(validate(|p| p.master_default.default = None), Ok(()));
}

#[test]
fn derived_references_resolve() {
    assert_eq!(
        validate(|p| p.derived = leak(vec![Derived {
            option: "nope",
            ..p.derived[0]
        }])),
        Err(ProfileError::UnknownOption { option: "nope" })
    );
    assert_eq!(
        validate(|p| p.derived = leak(vec![Derived {
            when_any: &["nope"],
            ..p.derived[0]
        }])),
        Err(ProfileError::UnknownTrigger { toggle: "nope" })
    );
    assert_eq!(
        validate(|p| p.derived = leak(vec![Derived {
            when_any: &[],
            ..p.derived[0]
        }])),
        Err(ProfileError::UnknownTrigger { toggle: "" })
    );
}

#[test]
fn every_bit_has_exactly_one_role_and_a_reader() {
    assert_eq!(
        validate(toggles(|t| t[0].controls = ControlSet::EMPTY)),
        Err(ProfileError::NoControls { toggle: "alpha" })
    );
    assert_eq!(
        validate(toggles(|t| t[1].controls = t[0].controls)),
        Err(ProfileError::SameControls {
            first: "alpha",
            second: "beta"
        })
    );
    assert_eq!(
        validate(toggles(
            |t| t[0].controls = ControlSet::of(bit(0)).with(bit(5))
        )),
        Err(ProfileError::UnusedBit { bit: bit(5) })
    );
    assert_eq!(
        validate(toggles(
            |t| t[0].controls = ControlSet::of(bit(0)).with(bit(31))
        )),
        Err(ProfileError::BitClash { bit: bit(31) }),
        "a derived bit is never selectable"
    );
    assert_eq!(
        validate(toggles(|t| t[2].controls = t[2].controls.with(bit(0)))),
        Ok(()),
        "a toggle may cover another's bits"
    );
    assert_eq!(
        validate(toggles(|t| {
            t.remove(1);
        })),
        Err(ProfileError::UnownedBit { bit: bit(1) })
    );
    assert_eq!(
        validate(sites(|s| {
            s[1].kind = HookKind::Gate {
                bit: bit(2),
                effect: GateEffect::SkipBatteryCall {
                    callee: Rva::new(0x1300),
                },
            };
        })),
        Err(ProfileError::BitClash { bit: bit(2) }),
        "a gate bit is not also a class bit"
    );
    assert_eq!(
        validate(sites(|s| {
            s[3].kind = HookKind::Pressure {
                bit: bit(30),
                kind: PressureKind::RecoveryRcx,
            };
        })),
        Err(ProfileError::UnusedBit { bit: bit(31) })
    );
    assert_eq!(
        validate(|p| {
            p.derived = leak(vec![p.derived[0], p.derived[0]]);
        }),
        Err(ProfileError::BitClash { bit: bit(31) })
    );
    assert_eq!(
        validate(|p| p.derived = &[]),
        Err(ProfileError::UnownedBit { bit: bit(31) })
    );
}

#[test]
fn classes_need_their_toggle_ids_and_sites() {
    assert_eq!(
        validate(classes(|c| c[1].bit = bit(2))),
        Err(ProfileError::BitClash { bit: bit(2) })
    );
    assert_eq!(
        validate(classes(|c| c[1].bit = bit(4))),
        Err(ProfileError::ClassToggle { class: "blue" })
    );
    assert_eq!(
        validate(toggles(|t| t[3].key = "green")),
        Err(ProfileError::ClassToggle { class: "red" })
    );
    assert_eq!(
        validate(classes(|c| c[0].interned_ids = &[])),
        Err(ProfileError::ClassIds { class: "red" })
    );
    assert_eq!(
        validate(classes(
            |c| c[1].interned_ids = leak(vec![Rva::new(0x2004), Rva::new(0x2000)])
        )),
        Err(ProfileError::ClassIds { class: "blue" })
    );
    assert_eq!(
        validate(classes(
            |c| c[1].interned_ids = leak(vec![Rva::new(0x2004), Rva::new(0x2004)])
        )),
        Err(ProfileError::ClassIds { class: "blue" })
    );
    assert_eq!(
        validate(classes(|c| c[1].key = "red")),
        Err(ProfileError::DuplicateKey { key: "red" })
    );
    assert_eq!(
        validate(sites(|s| {
            s.remove(2);
        })),
        Err(ProfileError::Classes)
    );
    assert_eq!(
        validate(|p| p.player.pressure_class = "green"),
        Err(ProfileError::PressureClass { class: "green" })
    );
}

#[test]
fn sites_are_bounded_covered_and_agree() {
    let id = SiteId(1);
    assert_eq!(
        validate(sites(|s| s[1].id = id)),
        Err(ProfileError::DuplicateSite { site: id })
    );
    assert_eq!(
        validate(sites(|s| s[0].original = &[0x48, 0x89, 0x5C, 0x24])),
        Err(ProfileError::ShortOriginal { site: id })
    );
    assert_eq!(
        validate(sites(
            |s| s[0].context.aob = crate::aob!("48 89 5C 24 08 57 48 83 EC 20 80 B9 F0 0B 00")
        )),
        Err(ProfileError::ShortContext { site: id })
    );
    assert_eq!(
        validate(sites(|s| {
            s[0].rva = Rva::new(0x1FFC);
            s[0].context.rva = Rva::new(0x1FF0);
        })),
        Err(ProfileError::OutsideText { site: id })
    );
    assert_eq!(
        validate(sites(|s| s[0].context.rva = Rva::new(0x0FFF))),
        Err(ProfileError::OutsideText { site: id })
    );
    assert_eq!(
        validate(sites(|s| s[0].context.rva = Rva::new(0x1101))),
        Err(ProfileError::ContextCoverage { site: id })
    );
    assert_eq!(
        validate(sites(|s| s[0].context.rva = Rva::new(0x10F4))),
        Err(ProfileError::ContextCoverage { site: id }),
        "the context must reach the original's last byte"
    );
    assert_eq!(
        validate(sites(|s| s[0].original = &[0x48, 0x89, 0x5C, 0x24, 0x09])),
        Err(ProfileError::ContextDisagrees {
            site: id,
            offset: 4
        })
    );
    assert_eq!(
        validate(sites(
            |s| s[3].original = &[0x40, 0x53, 0x48, 0x83, 0xEC, 0x21]
        )),
        Err(ProfileError::ContextDisagrees {
            site: SiteId(4),
            offset: 5
        })
    );
    assert_eq!(
        validate(sites(|s| s[1].rva = Rva::new(0x1104))),
        Err(ProfileError::ContextCoverage { site: SiteId(2) })
    );
}

#[test]
fn patched_ranges_never_overlap() {
    let shifted = |rva: u32| {
        sites(move |s| {
            s[0].rva = Rva::new(rva);
            s[0].context.rva = Rva::new(rva - 1);
            s[0].context.aob = crate::aob!("?? 48 89 5C 24 08 ?? ?? ?? ?? ?? ?? ?? ?? ?? ??");
        })
    };
    assert_eq!(
        validate(shifted(0x13FC)),
        Err(ProfileError::Overlap {
            first: SiteId(1),
            second: SiteId(3)
        })
    );
    assert_eq!(
        validate(shifted(0x13FB)),
        Ok(()),
        "touching is not overlapping"
    );
    assert_eq!(
        validate(shifted(0x1405)),
        Err(ProfileError::Overlap {
            first: SiteId(1),
            second: SiteId(3)
        })
    );
    assert_eq!(validate(shifted(0x1406)), Ok(()));
}

#[test]
fn call_effects_match_their_original_call() {
    let effect = |effect: GateEffect| {
        sites(move |s| {
            s[1].kind = HookKind::Gate {
                bit: bit(1),
                effect,
            };
        })
    };
    let site = SiteId(2);
    assert_eq!(
        validate(effect(GateEffect::SkipBatteryCall {
            callee: Rva::new(0x1301)
        })),
        Err(ProfileError::CallTarget { site })
    );
    assert_eq!(
        validate(effect(GateEffect::Throwable {
            callee: Rva::new(0x1300),
            enabled: Rva::new(0x1330)
        })),
        Ok(())
    );
    assert_eq!(
        validate(effect(GateEffect::Throwable {
            callee: Rva::new(0x1300),
            enabled: Rva::new(0x2000)
        })),
        Err(ProfileError::OutsideText { site })
    );
    assert_eq!(
        validate(effect(GateEffect::GodBranch {
            target: Rva::new(0x0FFF)
        })),
        Err(ProfileError::OutsideText { site })
    );
    assert_eq!(
        validate(sites(|s| {
            s[0].kind = HookKind::Gate {
                bit: bit(0),
                effect: GateEffect::SkipBatteryCall {
                    callee: Rva::new(0x1300),
                },
            };
        })),
        Err(ProfileError::CallTarget { site: SiteId(1) }),
        "the original must be a call"
    );
}

#[test]
fn class_toggles_and_concat_build_the_table() {
    let valid = ValidProfile::new(&SYNTHETIC).unwrap();
    let keys = valid.toggles().map(|(_, t)| t.key).collect::<Vec<_>>();
    assert_eq!(keys, ["alpha", "beta", "all_colours", "red", "blue"]);
    for (class_id, class) in valid.classes() {
        let toggle = valid.toggle(valid.class_toggle(class_id));
        assert_eq!(toggle.key, class.key);
        assert_eq!(toggle.controls, ControlSet::of(class.bit));
        assert_eq!(
            (toggle.since, toggle.binding.since),
            (class.since, class.since)
        );
        assert!(toggle.binding.default.is_none() && !toggle.default);
        assert_eq!(toggle.group, "colours");
    }
}

#[test]
fn lookups_mint_ids_in_display_order() {
    let valid = ValidProfile::new(&SYNTHETIC).unwrap();
    let blue = valid.toggle_id("blue").unwrap();
    assert_eq!((blue.index(), valid.toggle(blue).label), (4, "Blue"));
    assert_eq!(valid.toggle_id("Blue"), None);
    let keep = valid.option_id("keep").unwrap();
    assert_eq!(
        (keep.index(), valid.option(keep).label),
        (0, "Keep pressure")
    );
    assert_eq!(valid.option_id("alpha"), None);
    assert_eq!(
        valid
            .classes()
            .map(|(id, _)| id.index())
            .collect::<Vec<_>>(),
        [0, 1]
    );
}

#[test]
fn defaults_follow_the_profile() {
    let valid = ValidProfile::new(&SYNTHETIC).unwrap();
    let desired = valid.defaults();
    assert_eq!(desired.master, Master::Active);
    let selected = valid
        .toggles()
        .filter(|(id, _)| desired.selection.contains(*id))
        .map(|(_, t)| t.key)
        .collect::<Vec<_>>();
    assert_eq!(selected, ["beta"]);
    assert!(desired.options.contains(valid.option_id("keep").unwrap()));
    assert_eq!(valid.effective(&desired).bits().word(), 0b10);
    assert!(valid.no_selection().is_empty());
}

/// The rule written out for the synthetic profile, independently of the profile's data.
fn oracle(master: Master, selected: [bool; 5], keep: bool) -> u32 {
    let [alpha, beta, all, red, blue] = selected;
    if master == Master::Paused {
        return 0;
    }
    let mut word = 0;
    word |= u32::from(alpha);
    word |= u32::from(beta) << 1;
    word |= u32::from(all || red) << 2;
    word |= u32::from(all || blue) << 3;
    word |= u32::from(keep && (alpha || all || blue)) << 31;
    word
}

#[test]
fn effective_mask_matches_the_rule_for_every_input() {
    let valid = ValidProfile::new(&SYNTHETIC).unwrap();
    let ids = valid.toggles().map(|(id, _)| id).collect::<Vec<_>>();
    let keep = valid.option_id("keep").unwrap();
    for bits in 0u8..32 {
        let selected: [bool; 5] = std::array::from_fn(|i| bits & 1 << i != 0);
        for keep_on in [false, true] {
            for master in [Master::Active, Master::Paused] {
                let mut desired = valid.defaults();
                desired.master = master;
                desired.options.set(keep, keep_on);
                for (id, on) in ids.iter().zip(selected) {
                    desired.selection.set(*id, on);
                }
                assert_eq!(
                    valid.effective(&desired).bits().word(),
                    oracle(master, selected, keep_on),
                    "{selected:?} keep={keep_on} {master:?}"
                );
            }
        }
    }
}

#[test]
fn pausing_keeps_selections() {
    let valid = ValidProfile::new(&SYNTHETIC).unwrap();
    let mut desired = valid.defaults();
    let alpha = valid.toggle_id("alpha").unwrap();
    desired.selection.set(alpha, true);
    desired.master = desired.master.toggled();
    assert_eq!(valid.effective(&desired).bits().word(), 0);
    desired.master = desired.master.toggled();
    assert_eq!(valid.effective(&desired).bits().word(), 0x8000_0003);
}

fn other_profile() -> ValidProfile<'static> {
    let profile = Box::leak(Box::new(Profile {
        slug: ModSlug {
            game: "other-game",
            r#mod: "trainer",
        },
        ..SYNTHETIC
    }));
    ValidProfile::new(profile).unwrap()
}

#[test]
#[should_panic(expected = "another profile")]
fn ids_cannot_cross_profiles() {
    let other = other_profile();
    let id = other.toggle_id("alpha").unwrap();
    let _ = ValidProfile::new(&SYNTHETIC).unwrap().toggle(id);
}

#[test]
#[should_panic(expected = "another profile")]
fn selections_cannot_cross_profiles() {
    let other = other_profile();
    let _ = ValidProfile::new(&SYNTHETIC)
        .unwrap()
        .effective(&other.defaults());
}

#[test]
fn a_profile_mints_the_same_ids_each_time() {
    let first = ValidProfile::new(&SYNTHETIC).unwrap();
    let second = ValidProfile::new(&SYNTHETIC).unwrap();
    assert_eq!(first.toggle_id("beta"), second.toggle_id("beta"));
    assert_ne!(other_profile().toggle_id("beta"), first.toggle_id("beta"));
    assert_eq!(
        second.effective(&first.defaults()),
        first.effective(&first.defaults())
    );
}
