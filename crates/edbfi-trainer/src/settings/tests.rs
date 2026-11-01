// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]

use std::fs;
use std::path::Path;

use proptest::prelude::*;

use super::store::Hook;
use super::*;
use crate::chord::{Key, Modifiers};
use crate::profile::synthetic::SYNTHETIC;
use crate::profile::{BindingDefault, Profile, Toggle};
use crate::recovery::RecoveryAction;
use crate::testdir::TestDir;

fn synthetic() -> ValidProfile<'static> {
    ValidProfile::new(&SYNTHETIC).unwrap()
}

const SYNTHETIC_DEFAULTS: &str = r#"# Synthetic Game trainer settings. The trainer saves this file itself and does not keep comments.
schema = 1

[trainer]
start_paused = false
sounds = true

[cheats]
alpha = false
beta = true
all_colours = false

[colours]
red = false
blue = false

[options]
keep = true

[bindings]
master = "Alt+F8"
alpha = "Alt+F1"
beta = "Alt+F2"
all_colours = "Alt+1"
red = "None"
blue = "None"
"#;

/// Schema 2 of the synthetic profile: `beta` and the master's binding are new.
const V2_TOGGLES: [Toggle; 5] = {
    let t = SYNTHETIC.toggles;
    let beta = Toggle {
        since: 2,
        binding: BindingDefault {
            since: 2,
            ..t[1].binding
        },
        ..t[1]
    };
    [t[0], beta, t[2], t[3], t[4]]
};
const V2: Profile = Profile {
    toggles: &V2_TOGGLES,
    master_default: BindingDefault {
        since: 2,
        ..SYNTHETIC.master_default
    },
    settings_schema: 2,
    ..SYNTHETIC
};
const _: () = V2.check();

fn v2() -> ValidProfile<'static> {
    ValidProfile::new(&V2).unwrap()
}

fn alt(key: Option<Key>) -> Chord {
    Chord::new(Modifiers::ALT, key.unwrap())
}

fn error(text: &str) -> SettingsError {
    Settings::parse(&synthetic(), text.as_bytes()).unwrap_err()
}

/// The defaults with `from` replaced by `to`, which must occur exactly once.
fn edited(from: &str, to: &str) -> String {
    assert_eq!(SYNTHETIC_DEFAULTS.matches(from).count(), 1, "{from}");
    SYNTHETIC_DEFAULTS.replacen(from, to, 1)
}

fn at(line: u32, column: u32) -> Option<Location> {
    Some(Location { line, column })
}

#[test]
fn defaults_write_the_canonical_file_and_read_back() {
    let profile = synthetic();
    let defaults = Settings::defaults(&profile);
    assert_eq!(defaults.to_toml(&profile), SYNTHETIC_DEFAULTS);
    let parsed = Settings::parse(&profile, SYNTHETIC_DEFAULTS.as_bytes()).unwrap();
    assert_eq!(
        parsed,
        Parsed {
            settings: defaults,
            upgrade: None
        }
    );
}

#[test]
fn first_run_is_active_with_profile_defaults() {
    let profile = synthetic();
    let defaults = Settings::defaults(&profile);
    assert_eq!(defaults.master_on_connect(), Master::Active);
    assert!(defaults.sounds);
    let beta = profile.toggle_id("beta").unwrap();
    let alpha = profile.toggle_id("alpha").unwrap();
    assert!(defaults.selection.contains(beta));
    assert!(!defaults.selection.contains(alpha));
    let paused = Settings {
        start_paused: true,
        ..defaults
    };
    assert_eq!(paused.master_on_connect(), Master::Paused);
}

#[test]
fn every_value_reads_back() {
    let profile = synthetic();
    let text = edited("start_paused = false", "start_paused = true")
        .replacen("sounds = true", "sounds = false", 1)
        .replacen("red = false", "red = true", 1)
        .replacen("keep = true", "keep = false", 1)
        .replacen("red = \"None\"", "red = \"Ctrl+Shift+Num5\"", 1)
        .replacen("alpha = \"Alt+F1\"", "alpha = \"None\"", 1);
    let settings = Settings::parse(&profile, text.as_bytes()).unwrap().settings;
    assert!(settings.start_paused && !settings.sounds);
    assert!(
        settings
            .selection
            .contains(profile.toggle_id("red").unwrap())
    );
    assert!(
        !settings
            .options
            .contains(profile.option_id("keep").unwrap())
    );
    let red = Target::Toggle(profile.toggle_id("red").unwrap());
    assert_eq!(settings.bindings.get(red).to_string(), "Ctrl+Shift+Num5");
    let alpha = Target::Toggle(profile.toggle_id("alpha").unwrap());
    assert_eq!(settings.bindings.get(alpha), Binding(None));
    assert_eq!(settings.to_toml(&profile), text);
}

#[test]
fn hand_written_layout_and_one_bom_are_accepted() {
    let profile = synthetic();
    let rest = SYNTHETIC_DEFAULTS[SYNTHETIC_DEFAULTS.find("[trainer]").unwrap()..].replacen(
        "[cheats]\nalpha = false\nbeta = true\nall_colours = false\n",
        "",
        1,
    );
    let text = format!(
        "\u{FEFF}# mine\nschema = 1\ncheats = {{ alpha = true, beta = true, all_colours = false }}\n{rest}"
    );
    let settings = Settings::parse(&profile, text.as_bytes()).unwrap().settings;
    assert!(
        settings
            .selection
            .contains(profile.toggle_id("alpha").unwrap())
    );

    let twice = format!("\u{FEFF}\u{FEFF}{SYNTHETIC_DEFAULTS}");
    let found = Settings::parse(&profile, twice.as_bytes()).unwrap_err();
    assert_eq!(found.problem, Problem::SecondBom);
    assert_eq!(found.location, at(1, 1));
}

#[test]
fn size_and_encoding_are_checked_first() {
    let big = format!("{SYNTHETIC_DEFAULTS}{}", "#".repeat(MAX_BYTES));
    assert_eq!(error(&big).problem, Problem::TooLarge);
    let mut bytes = SYNTHETIC_DEFAULTS.as_bytes().to_vec();
    let at_beta = SYNTHETIC_DEFAULTS.find("beta = true").unwrap();
    bytes[at_beta + 1] = 0xFF;
    let found = Settings::parse(&synthetic(), &bytes).unwrap_err();
    assert_eq!(found.problem, Problem::NotUtf8);
    assert_eq!(found.location, at(10, 2));
}

#[test]
fn syntax_errors_carry_line_and_column() {
    let found = error(&edited("alpha = false", "alpha = tru"));
    assert!(matches!(found.problem, Problem::Syntax { .. }), "{found}");
    assert_eq!(found.location.map(|l| l.line), Some(9));
    let found = error(&edited("beta = true\n", "beta = true\nbeta = false\n"));
    assert!(matches!(found.problem, Problem::Syntax { .. }), "{found}");
    assert_eq!(found.location.map(|l| l.line), Some(11));
    assert_eq!(found.recovery(), RecoveryAction::EditSettings);
}

#[test]
fn the_schema_must_be_present_and_supported() {
    let found = error(&edited("schema = 1\n", ""));
    assert_eq!(found.problem, Problem::MissingSchema { supported: 1 });
    for bad in [
        "schema = 0",
        "schema = -1",
        "schema = \"1\"",
        "schema = 1.0",
    ] {
        let found = error(&edited("schema = 1", bad));
        assert_eq!(found.problem, Problem::BadSchema { supported: 1 }, "{bad}");
        assert_eq!(found.location, at(2, 10), "{bad}");
    }
    let found = error(&edited("schema = 1", "schema = 2"));
    assert_eq!(
        found.problem,
        Problem::NewerSchema {
            found: 2,
            supported: 1
        }
    );
    assert_eq!(found.recovery(), RecoveryAction::ResetSettings);
}

#[test]
fn unknown_and_missing_entries_are_refused() {
    let cases: [(String, Problem, Option<Location>); 7] = [
        (
            edited("[options]", "[extras]\nx = 1\n\n[options]"),
            Problem::UnknownTable {
                table: "extras".into(),
            },
            at(17, 2),
        ),
        (
            edited("schema = 1\n", "schema = 1\nverbose = true\n"),
            Problem::UnknownKey {
                table: String::new(),
                key: "verbose".into(),
            },
            at(3, 1),
        ),
        (
            edited("beta = true", "beta = true\ngamma = true"),
            Problem::UnknownKey {
                table: "cheats".into(),
                key: "gamma".into(),
            },
            at(11, 1),
        ),
        (
            edited("[options]\nkeep = true\n", ""),
            Problem::MissingTable {
                table: "options".into(),
            },
            None,
        ),
        (
            edited("beta = true\n", ""),
            Problem::MissingKey {
                table: "cheats".into(),
                key: "beta".into(),
            },
            at(8, 2),
        ),
        (
            edited("[options]\nkeep = true\n", "").replacen(
                "schema = 1\n",
                "schema = 1\noptions = 1\n",
                1,
            ),
            Problem::NotATable {
                table: "options".into(),
            },
            at(3, 1),
        ),
        (
            edited("alpha = false", "alpha = 0"),
            Problem::WrongType {
                table: "cheats".into(),
                key: "alpha".into(),
                expected: Expected::Bool,
            },
            at(9, 9),
        ),
    ];
    for (text, problem, location) in cases {
        let found = error(&text);
        assert_eq!(found.problem, problem, "{text}");
        assert_eq!(found.location, location, "{found}");
    }
}

#[test]
fn bindings_are_parsed_policed_and_unique() {
    let found = error(&edited("alpha = \"Alt+F1\"", "alpha = 1"));
    assert!(matches!(
        found.problem,
        Problem::WrongType {
            expected: Expected::Binding,
            ..
        }
    ));
    let found = error(&edited("alpha = \"Alt+F1\"", "alpha = \"alt+F1\""));
    assert_eq!(
        found.to_string(),
        "line 22, column 9: alpha in [bindings]: unknown modifier \"alt\"; did you mean Alt?"
    );
    let found = error(&edited("alpha = \"Alt+F1\"", "alpha = \"Alt+F4\""));
    assert!(matches!(
        found.problem,
        Problem::RefusedBinding {
            refusal: Refusal::System,
            ..
        }
    ));
    let found = error(&edited("red = \"None\"", "red = \"Alt+F1\""));
    assert_eq!(
        found.problem,
        Problem::DuplicateBinding {
            key: "red".into(),
            first: "alpha".into(),
            chord: "Alt+F1".into(),
        }
    );
    assert_eq!(found.location, at(25, 1));
}

#[test]
fn the_earliest_problem_is_reported() {
    let text = edited("blue = \"None\"", "blue = 3").replacen("alpha = false", "alpha = 3", 1);
    assert_eq!(error(&text).location, at(9, 9));
}

#[test]
fn names_from_the_file_are_escaped() {
    let found = error(&edited(
        "schema = 1\n",
        "schema = 1\n\"\\u001b[2J\\u202e\" = 1\n",
    ));
    let message = found.to_string();
    assert!(message.contains(r#""\u{1b}[2J\u{202e}""#), "{message}");
    assert!(!message.contains('\u{1b}'));
}

#[test]
fn schema_keys_are_listed_per_schema() {
    let profile = v2();
    let keys = |schema| -> Vec<String> {
        Settings::keys(&profile, schema)
            .map(|(table, key)| format!("{table}.{key}"))
            .collect()
    };
    let v1 = keys(1);
    let v2 = keys(2);
    assert_eq!(v2.len(), v1.len() + 3);
    for added in ["cheats.beta", "bindings.beta", "bindings.master"] {
        assert!(v2.iter().any(|key| key == added) && !v1.iter().any(|key| key == added));
    }
    assert_eq!(v1[0], ".schema");
}

/// A schema-1 file of the synthetic profile, which is what [`V2`] calls schema 1.
fn v1_file(alpha_binding: &str) -> String {
    let profile = v2();
    let keys: Vec<(&str, &str)> = Settings::keys(&profile, 1).collect();
    let text = edited("schema = 1", "schema = 1")
        .replacen("beta = true\n", "", 1)
        .replacen("master = \"Alt+F8\"\n", "", 1)
        .replacen("beta = \"Alt+F2\"\n", "", 1)
        .replacen(
            "alpha = \"Alt+F1\"",
            &format!("alpha = \"{alpha_binding}\""),
            1,
        );
    assert_eq!(text.matches(" = ").count(), keys.len());
    text
}

#[test]
fn an_older_schema_gains_only_new_defaults() {
    let profile = v2();
    let parsed = Settings::parse(&profile, v1_file("Alt+F1").as_bytes()).unwrap();
    assert_eq!(
        parsed.upgrade,
        Some(Upgrade {
            from: 1,
            unbound: vec![]
        })
    );
    let beta = profile.toggle_id("beta").unwrap();
    assert!(parsed.settings.selection.contains(beta));
    assert_eq!(
        parsed.settings.bindings.get(Target::Master),
        Binding(Some(alt(Key::function(8))))
    );
    let written = parsed.settings.to_toml(&profile);
    assert!(written.starts_with("# Synthetic Game trainer settings."));
    assert!(written.contains("schema = 2\n"));
    let again = Settings::parse(&profile, written.as_bytes()).unwrap();
    assert_eq!(again.upgrade, None);
    assert_eq!(again.settings, parsed.settings);
}

#[test]
fn a_new_default_whose_chord_is_taken_starts_unbound() {
    let profile = v2();
    let parsed = Settings::parse(&profile, v1_file("Alt+F8").as_bytes()).unwrap();
    let upgrade = parsed.upgrade.unwrap();
    assert_eq!(upgrade.unbound, vec![Target::Master]);
    assert_eq!(parsed.settings.bindings.get(Target::Master), Binding(None));
    let alpha = Target::Toggle(profile.toggle_id("alpha").unwrap());
    assert_eq!(
        parsed.settings.bindings.get(alpha),
        Binding(Some(alt(Key::function(8))))
    );
}

#[test]
fn an_older_schema_still_needs_every_old_key_and_no_new_one() {
    let profile = v2();
    let missing = v1_file("Alt+F1").replacen("alpha = false\n", "", 1);
    let found = Settings::parse(&profile, missing.as_bytes()).unwrap_err();
    assert!(matches!(found.problem, Problem::MissingKey { .. }));
    let early = v1_file("Alt+F1").replacen("[bindings]\n", "[bindings]\nmaster = \"Alt+F8\"\n", 1);
    let found = Settings::parse(&profile, early.as_bytes()).unwrap_err();
    assert_eq!(
        found.problem,
        Problem::UnknownKey {
            table: "bindings".into(),
            key: "master".into()
        }
    );
}

#[test]
fn assigning_bindings_keeps_them_unique_and_allowed() {
    let profile = synthetic();
    let mut bindings = Bindings::defaults(&profile);
    let alpha = Target::Toggle(profile.toggle_id("alpha").unwrap());
    let red = Target::Toggle(profile.toggle_id("red").unwrap());
    let f1 = Binding(Some(alt(Key::function(1))));
    assert_eq!(
        bindings.assign(red, f1),
        Err(AssignError::Taken { by: alpha })
    );
    assert_eq!(bindings.assign(alpha, f1), Ok(f1));
    let refused = Binding(Some(Chord::new(
        Modifiers::NONE,
        Key::letter(b'Q').unwrap(),
    )));
    assert_eq!(
        bindings.assign(red, refused),
        Err(AssignError::Refused(Refusal::Typing))
    );
    assert_eq!(bindings.assign(alpha, Binding(None)), Ok(f1));
    assert_eq!(bindings.assign(red, f1), Ok(Binding(None)));
    assert_eq!(bindings.holder(alt(Key::function(1))), Some(red));
}

fn arbitrary_settings() -> impl Strategy<Value = Settings> {
    let profile = synthetic();
    let chords: Vec<Binding> = std::iter::once(Binding(None))
        .chain(
            (1..=24)
                .filter(|n| *n != 4)
                .map(|n| Binding(Some(alt(Key::function(n))))),
        )
        .chain((0..=9).map(|n| {
            Binding(Some(Chord::new(
                Modifiers::CTRL.union(Modifiers::SHIFT),
                Key::numpad(n).unwrap(),
            )))
        }))
        .collect();
    let count = profile.toggles().len() + 1;
    (
        any::<bool>(),
        any::<bool>(),
        any::<u8>(),
        any::<bool>(),
        proptest::collection::vec(proptest::sample::select(chords), count),
    )
        .prop_map(move |(start_paused, sounds, picks, keep, wanted)| {
            let mut settings = Settings::defaults(&profile);
            settings.start_paused = start_paused;
            settings.sounds = sounds;
            for (id, _) in profile.toggles() {
                settings.selection.set(id, picks & 1 << id.index() != 0);
            }
            settings
                .options
                .set(profile.option_id("keep").unwrap(), keep);
            let targets: Vec<Target> = settings.bindings.iter().map(|(t, _)| t).collect();
            for target in &targets {
                settings.bindings.set(*target, Binding(None));
            }
            for (target, binding) in targets.into_iter().zip(wanted) {
                let _taken = settings.bindings.assign(target, binding);
            }
            settings
        })
}

proptest! {
    #[test]
    fn any_settings_round_trip(settings in arbitrary_settings()) {
        let profile = synthetic();
        let text = settings.to_toml(&profile);
        let parsed = Settings::parse(&profile, text.as_bytes()).unwrap();
        prop_assert_eq!(parsed.upgrade, None);
        prop_assert_eq!(&parsed.settings, &settings);
        prop_assert_eq!(parsed.settings.to_toml(&profile), text);
    }

    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _result = Settings::parse(&synthetic(), &bytes);
    }

    #[test]
    fn damaged_files_fail_with_a_place_or_parse(cut in 0..SYNTHETIC_DEFAULTS.len(), len in 0..40usize) {
        let end = (cut + len).min(SYNTHETIC_DEFAULTS.len());
        let (Some(head), Some(tail)) = (SYNTHETIC_DEFAULTS.get(..cut), SYNTHETIC_DEFAULTS.get(end..)) else {
            return Ok(());
        };
        let text = format!("{head}{tail}");
        if let Err(error) = Settings::parse(&synthetic(), text.as_bytes()) {
            prop_assert!(!error.to_string().is_empty());
        }
    }
}

// The store.

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

fn modified(profile: &ValidProfile<'_>) -> Settings {
    let mut settings = Settings::defaults(profile);
    settings
        .selection
        .set(profile.toggle_id("alpha").unwrap(), true);
    settings
}

fn loaded(dir: &TestDir) -> Store {
    let mut store = Store::open(dir.path()).unwrap();
    store.load(&synthetic()).unwrap();
    store
}

#[test]
fn a_missing_file_is_created_with_the_defaults() {
    let dir = TestDir::new("settings-create");
    let folder = dir
        .path()
        .join("edbfi")
        .join("synthetic-game")
        .join("trainer");
    let mut store = Store::open(&folder).unwrap();
    let profile = synthetic();
    assert_eq!(
        store.load(&profile).unwrap(),
        Load::Created(Settings::defaults(&profile))
    );
    assert_eq!(read(store.path()), SYNTHETIC_DEFAULTS);
    assert_eq!(
        store.load(&profile).unwrap(),
        Load::Read(Parsed {
            settings: Settings::defaults(&profile),
            upgrade: None,
        })
    );
}

#[test]
fn a_second_writer_is_refused() {
    let dir = TestDir::new("settings-lease");
    let _first = Store::open(dir.path()).unwrap();
    let second = Store::open(dir.path()).unwrap_err();
    assert!(matches!(second, StoreError::Leased { .. }));
    assert_eq!(second.recovery(), RecoveryAction::CloseOtherTrainer);
    assert!(!second.allows_session_only());
    drop(_first);
    Store::open(dir.path()).unwrap();
}

#[test]
fn an_unreadable_file_blocks_and_a_missing_folder_allows_session_only() {
    let dir = TestDir::new("settings-unreadable");
    fs::create_dir(dir.path().join("settings.toml")).unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let found = store.load(&synthetic()).unwrap_err();
    assert!(matches!(found, StoreError::Unreadable { .. }), "{found}");
    assert!(!found.allows_session_only());

    let file = dir.path().join("plain-file");
    fs::write(&file, "").unwrap();
    let found = Store::open(&file.join("below")).unwrap_err();
    assert!(matches!(found, StoreError::Folder { .. }), "{found}");
    assert!(found.allows_session_only());
}

#[test]
fn an_invalid_file_is_reported_and_reset_keeps_every_backup() {
    let dir = TestDir::new("settings-reset");
    let profile = synthetic();
    let path = dir.path().join("settings.toml");
    fs::write(&path, "schema = 9\n").unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let Load::Invalid(found) = store.load(&profile).unwrap() else {
        panic!("not invalid");
    };
    assert!(matches!(
        found.problem,
        Problem::NewerSchema { found: 9, .. }
    ));

    let reset = store.reset(&profile).unwrap();
    assert_eq!(reset.settings, Settings::defaults(&profile));
    assert_eq!(reset.external, None);
    let first = reset.backup.unwrap();
    assert_eq!(first, dir.path().join("settings.invalid-1.toml"));
    assert_eq!(read(&first), "schema = 9\n");
    assert_eq!(read(&path), SYNTHETIC_DEFAULTS);

    let second = store.reset(&profile).unwrap().backup.unwrap();
    assert_eq!(second, dir.path().join("settings.invalid-2.toml"));
    assert_eq!(read(&first), "schema = 9\n");
    assert_eq!(read(&second), SYNTHETIC_DEFAULTS);
    assert_eq!(
        dir.names(),
        [
            "settings.invalid-1.toml",
            "settings.invalid-2.toml",
            "settings.lock",
            "settings.toml"
        ]
    );
}

#[test]
fn saving_over_an_invalid_file_keeps_it() {
    let dir = TestDir::new("settings-invalid-save");
    let profile = synthetic();
    fs::write(dir.path().join("settings.toml"), "schema = 9\n").unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    assert!(matches!(store.load(&profile).unwrap(), Load::Invalid(_)));
    let kept = dir.path().join("settings.external-1.toml");
    assert_eq!(
        store.save(&profile, &modified(&profile)).unwrap(),
        Saved::Written {
            external: Some(External::Kept(kept.clone()))
        }
    );
    assert_eq!(read(&kept), "schema = 9\n");
}

#[test]
fn reset_and_save_keep_an_oversized_file_whole() {
    let dir = TestDir::new("settings-oversized");
    let profile = synthetic();
    let path = dir.path().join("settings.toml");
    let big = format!("{SYNTHETIC_DEFAULTS}{}\n", "#".repeat(MAX_BYTES + 4096));
    fs::write(&path, &big).unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    assert!(matches!(store.load(&profile).unwrap(), Load::Invalid(_)));
    let saved = store.save(&profile, &modified(&profile)).unwrap();
    let Saved::Written {
        external: Some(External::Kept(kept)),
    } = saved
    else {
        panic!("an unread version was not kept: {saved:?}");
    };
    assert_eq!(read(&kept), big);

    fs::write(&path, &big).unwrap();
    let mut store = {
        drop(store);
        Store::open(dir.path()).unwrap()
    };
    store.load(&profile).unwrap();
    let reset = store.reset(&profile).unwrap();
    assert_eq!(read(&reset.backup.unwrap()), big);
    assert_eq!(reset.external, None);
    assert_eq!(read(&path), SYNTHETIC_DEFAULTS);
}

#[test]
fn saving_writes_only_changes() {
    let dir = TestDir::new("settings-save");
    let profile = synthetic();
    let mut store = loaded(&dir);
    assert_eq!(
        store.save(&profile, &Settings::defaults(&profile)).unwrap(),
        Saved::Unchanged
    );
    let settings = modified(&profile);
    assert_eq!(
        store.save(&profile, &settings).unwrap(),
        Saved::Written { external: None }
    );
    assert_eq!(read(store.path()), settings.to_toml(&profile));
    assert_eq!(dir.names(), ["settings.lock", "settings.toml"]);
}

#[test]
fn an_external_edit_is_kept_and_reported() {
    let dir = TestDir::new("settings-external");
    let profile = synthetic();
    let mut store = loaded(&dir);
    fs::write(store.path(), "# edited by hand\n").unwrap();
    let saved = store.save(&profile, &modified(&profile)).unwrap();
    let kept = dir.path().join("settings.external-1.toml");
    assert_eq!(
        saved,
        Saved::Written {
            external: Some(External::Kept(kept.clone()))
        }
    );
    assert_eq!(read(&kept), "# edited by hand\n");
    assert_eq!(read(store.path()), modified(&profile).to_toml(&profile));

    fs::remove_file(store.path()).unwrap();
    let mut again = modified(&profile);
    again.sounds = false;
    assert_eq!(
        store.save(&profile, &again).unwrap(),
        Saved::Written {
            external: Some(External::Deleted)
        }
    );
    assert_eq!(read(store.path()), again.to_toml(&profile));
}

/// Every file in `dir` except the lock, with its contents.
fn contents(dir: &TestDir) -> Vec<(String, String)> {
    dir.names()
        .into_iter()
        .filter(|name| name != "settings.lock")
        .map(|name| {
            let text = read(&dir.path().join(&name));
            (name, text)
        })
        .collect()
}

fn fail_call() -> Hook {
    Box::new(|| Some(std::io::Error::other("injected")))
}

#[test]
fn a_save_failing_at_any_call_leaves_the_old_or_new_file() {
    let profile = synthetic();
    let (old, new) = (
        SYNTHETIC_DEFAULTS.to_owned(),
        modified(&profile).to_toml(&profile),
    );
    for at in 1.. {
        let dir = TestDir::new("settings-fault");
        let mut store = loaded(&dir);
        store.before_call(at, fail_call());
        let saved = store.save(&profile, &modified(&profile));
        let calls = store.calls();
        let expected = if saved.is_ok() { &new } else { &old };
        assert_eq!(&read(store.path()), expected, "fault at call {at}");
        for (name, text) in contents(&dir) {
            assert!(
                text == old || text == new,
                "fault at call {at}: {name} holds {text:?}"
            );
        }
        store.before_call(0, fail_call());
        assert!(store.save(&profile, &modified(&profile)).is_ok());
        assert_eq!(read(store.path()), new);
        if calls < at {
            break;
        }
    }
}

#[test]
fn an_edit_racing_a_save_at_any_call_is_never_lost() {
    let profile = synthetic();
    let theirs = "# theirs\n";
    for at in 1.. {
        let dir = TestDir::new("settings-race");
        let mut store = loaded(&dir);
        let path = store.path().to_owned();
        store.before_call(
            at,
            Box::new(move || {
                fs::write(&path, theirs).unwrap();
                None
            }),
        );
        let saved = store.save(&profile, &modified(&profile));
        let calls = store.calls();
        let found = contents(&dir);
        if calls >= at {
            assert!(
                found.iter().any(|(_, text)| text == theirs),
                "edit before call {at} lost: {found:?}"
            );
        }
        let on_disk = read(store.path());
        if let Ok(Saved::Written { external }) = &saved
            && on_disk != theirs
            && calls >= at
        {
            let Some(External::Kept(kept)) = external else {
                panic!("edit before call {at} not reported: {saved:?}");
            };
            assert_eq!(read(kept), theirs);
        }
        if calls < at {
            break;
        }
    }
}

#[test]
fn a_file_created_while_loading_is_read_not_replaced() {
    let profile = synthetic();
    let theirs = modified(&profile).to_toml(&profile);
    for at in 1.. {
        let dir = TestDir::new("settings-create-race");
        let mut store = Store::open(dir.path()).unwrap();
        let (path, text) = (store.path().to_owned(), theirs.clone());
        store.before_call(
            at,
            Box::new(move || {
                let _exists = fs::write(&path, &text);
                None
            }),
        );
        let load = store.load(&profile).unwrap();
        let calls = store.calls();
        if calls >= at {
            assert_eq!(read(store.path()), theirs, "creation before call {at}");
            if let Load::Created(_) = load {
                // Ours was written first and theirs overwrote it in place; the next save keeps theirs.
                let mut quiet = Settings::defaults(&profile);
                quiet.sounds = false;
                let saved = store.save(&profile, &quiet).unwrap();
                assert!(matches!(
                    saved,
                    Saved::Written {
                        external: Some(External::Kept(_))
                    }
                ));
            } else {
                assert_eq!(
                    load,
                    Load::Read(Parsed {
                        settings: modified(&profile),
                        upgrade: None
                    })
                );
            }
        }
        if calls < at {
            break;
        }
    }
}

#[test]
fn a_reset_failing_at_any_call_keeps_the_old_file() {
    let profile = synthetic();
    let old = "schema = 9\n";
    for at in 1.. {
        let dir = TestDir::new("settings-reset-fault");
        fs::write(dir.path().join("settings.toml"), old).unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        store.load(&profile).unwrap();
        store.before_call(at, fail_call());
        let reset = store.reset(&profile);
        let calls = store.calls();
        let expected = if reset.is_ok() {
            SYNTHETIC_DEFAULTS
        } else {
            old
        };
        assert_eq!(read(store.path()), expected, "fault at call {at}");
        for (name, text) in contents(&dir) {
            assert!(
                text == old || text == SYNTHETIC_DEFAULTS,
                "fault at call {at}: {name} holds {text:?}"
            );
        }
        if calls < at {
            break;
        }
    }
}
