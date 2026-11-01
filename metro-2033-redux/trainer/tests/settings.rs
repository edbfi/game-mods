// SPDX-License-Identifier: AGPL-3.0-only
//! The Metro settings file: schema 1's exact keys and first-run contents. Changing either needs a schema bump,
//! and removing or renaming a key is a breaking change.
#![expect(
    clippy::unwrap_used,
    reason = "helpers shared by tests; an invalid profile is a test failure"
)]

use edbfi_trainer::location::Locations;
use edbfi_trainer::profile::ValidProfile;
use edbfi_trainer::settings::Settings;
use metro_2033_redux_trainer::profile::METRO;

fn valid() -> ValidProfile<'static> {
    ValidProfile::new(&METRO).unwrap()
}

/// Schema 1, written on first run.
const SCHEMA_1_DEFAULTS: &str = r#"# Metro 2033 Redux trainer settings. The trainer saves this file itself and does not keep comments.
schema = 1

[trainer]
start_paused = false
sounds = true

[cheats]
unlimited_ammo = false
unlimited_throwables = false
unlimited_medkits = false
unlimited_filters = false
indestructible_mask = false
no_reload = false
god_mode = false
unlimited_flashlight = false

[weapons]
ak74 = false
aksu = false
rpk = false
ashot = false
vsv = false
ventil = false
padonag = false
preved = false
vyhlop = false
saiga = false
tumak = false
2012 = false
gatling = false
ubludok = false
uboynicheg = false
bigun = false
duplet = false
revolver = false
abzac = false
hellbreath = false
hellshock = false
tihar = false
helsing = false
flamethrower = false

[options]
keep_hellsing_pressure = true

[bindings]
master = "Alt+F8"
unlimited_ammo = "Alt+F1"
unlimited_throwables = "Alt+F2"
unlimited_medkits = "Alt+F3"
unlimited_filters = "Alt+F6"
indestructible_mask = "Alt+F5"
no_reload = "Alt+1"
god_mode = "Alt+2"
unlimited_flashlight = "Alt+3"
ak74 = "None"
aksu = "None"
rpk = "None"
ashot = "None"
vsv = "None"
ventil = "None"
padonag = "None"
preved = "None"
vyhlop = "None"
saiga = "None"
tumak = "None"
2012 = "None"
gatling = "None"
ubludok = "None"
uboynicheg = "None"
bigun = "None"
duplet = "None"
revolver = "None"
abzac = "None"
hellbreath = "None"
hellshock = "None"
tihar = "None"
helsing = "None"
flamethrower = "None"
"#;

#[test]
fn first_run_writes_schema_1_defaults() {
    let profile = valid();
    assert_eq!(METRO.settings_schema, 1);
    assert_eq!(
        Settings::defaults(&profile).to_toml(&profile),
        SCHEMA_1_DEFAULTS
    );
    let parsed = Settings::parse(&profile, SCHEMA_1_DEFAULTS.as_bytes()).unwrap();
    assert_eq!(parsed.upgrade, None);
    assert_eq!(parsed.settings, Settings::defaults(&profile));
}

#[test]
fn schema_1_has_exactly_the_pinned_keys() {
    let pinned: Vec<String> = SCHEMA_1_DEFAULTS
        .lines()
        .scan(String::new(), |table, line| {
            if let Some(name) = line.strip_prefix('[') {
                *table = name.trim_end_matches(']').to_owned();
            }
            Some(
                line.split_once(" = ")
                    .map(|(key, _)| format!("{table}.{key}")),
            )
        })
        .flatten()
        .collect();
    let keys: Vec<String> = Settings::keys(&valid(), 1)
        .map(|(table, key)| format!("{table}.{key}"))
        .collect();
    assert_eq!(keys, pinned);
    assert_eq!(keys.len(), 1 + 2 + 8 + 24 + 1 + 1 + 32);
}

#[test]
fn settings_and_logs_live_in_the_mod_folders() {
    let (roaming, local) = (
        std::env::temp_dir().join("Roaming"),
        std::env::temp_dir().join("Local"),
    );
    let found = Locations::under(METRO.slug, &roaming, &local);
    assert_eq!(
        found.settings,
        roaming
            .join("edbfi")
            .join("metro-2033-redux")
            .join("trainer")
    );
    assert_eq!(
        found.logs,
        local.join("edbfi").join("metro-2033-redux").join("trainer")
    );
}
