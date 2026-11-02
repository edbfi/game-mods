// SPDX-License-Identifier: AGPL-3.0-only
//! The reducer against the synthetic profile: status words, publication and acknowledgement, persistence,
//! dialogs and closed input.

use std::path::PathBuf;
use std::time::Duration;

use edbfi_trainer::chord::{Chord, Key, Modifiers};
use edbfi_trainer::control::{Ack, DesiredMask, Master};
use edbfi_trainer::input::{Command, ConsoleKey, Press};
use edbfi_trainer::lifecycle::{Failure, Phase, RuntimeCommand, RuntimeEvent, SettingsProblem};
use edbfi_trainer::profile::ValidProfile;
use edbfi_trainer::profile::synthetic::SYNTHETIC;
use edbfi_trainer::recovery::RecoveryAction;
use edbfi_trainer::settings::{Saved, Settings, StoreError, Target};

use super::{App, Chime, Effect, Modal, Msg, Paths, Row, Startup, Status};
use crate::theme::Appearance;

fn profile() -> ValidProfile<'static> {
    ValidProfile::new(&SYNTHETIC).unwrap()
}

fn startup(phase: Phase) -> Startup {
    Startup {
        settings: Settings::defaults(&profile()),
        paths: Paths {
            settings: Some(PathBuf::from("settings.toml")),
            log: Some(PathBuf::from("trainer.log")),
        },
        phase,
        appearance: Appearance::default(),
        size: (100, 30),
    }
}

fn app(phase: Phase) -> App<'static> {
    App::new(profile(), startup(phase))
}

fn attached() -> Phase {
    Phase::Attached {
        attach_duration: Duration::from_millis(5),
    }
}

fn id(key: &str) -> Row {
    Row::Toggle(profile().toggle_id(key).unwrap())
}

fn target(key: &str) -> Target {
    Target::Toggle(profile().toggle_id(key).unwrap())
}

fn press(key: ConsoleKey) -> Msg {
    Msg::Input(Command::Press(Press::plain(key)))
}

fn letter(byte: u8) -> Msg {
    press(ConsoleKey::Key(Key::letter(byte).unwrap()))
}

fn published(effects: &[Effect]) -> Option<DesiredMask> {
    effects.iter().find_map(|effect| match effect {
        Effect::Runtime(RuntimeCommand::Publish(mask)) => Some(*mask),
        _ => None,
    })
}

fn ack(app: &mut App<'static>, revision: u64, mask: DesiredMask) -> Vec<Effect> {
    app.update(Msg::Runtime(RuntimeEvent::Ack(Ack::simulated(
        revision, mask,
    ))))
}

/// Attaches and acknowledges the first publication.
fn live() -> App<'static> {
    let mut app = app(Phase::Waiting);
    let effects = app.update(Msg::Runtime(RuntimeEvent::Phase(attached())));
    let mask = published(&effects).expect("attaching publishes");
    ack(&mut app, 1, mask);
    app
}

#[test]
fn rows_show_acknowledged_state_never_the_wish() {
    let mut app = app(Phase::Waiting);
    assert_eq!(
        app.status(id("beta")),
        Status::Armed,
        "selected before attach"
    );
    assert_eq!(app.status(id("alpha")), Status::Off);
    assert_eq!(app.status(Row::Master), Status::Inactive);
    let effects = app.update(Msg::Runtime(RuntimeEvent::Phase(attached())));
    let mask = published(&effects).unwrap();
    assert_eq!(app.status(id("beta")), Status::Off, "not yet acknowledged");
    assert!(app.pending(id("beta")));
    ack(&mut app, 1, mask);
    assert_eq!(app.status(id("beta")), Status::On);
    assert!(!app.pending(id("beta")));
    assert_eq!(app.status(Row::Master), Status::Active);
}

#[test]
fn a_covered_class_reads_via_and_its_own_choice_stays_visible() {
    let mut app = live();
    app.update(Msg::Input(Command::Hotkey(target("all_colours"))));
    let mask = app.profile.effective(&app.desired());
    ack(&mut app, 2, mask);
    assert_eq!(app.status(id("red")), Status::Via);
    assert_eq!(app.status(id("all_colours")), Status::On);
    assert!(!app.marked(id("red")), "red itself is not selected");
    // Selecting red under the global changes nothing published, so nothing is sent or chimed.
    app.model.focus = id("red");
    let effects = app.update(press(ConsoleKey::Enter));
    assert_eq!(published(&effects), None);
    assert!(effects.contains(&Effect::Save));
    assert_eq!(app.status(id("red")), Status::On);
    assert!(app.marked(id("red")));
}

#[test]
fn stale_or_repeated_acks_are_ignored_and_chimes_follow_changes_only() {
    let mut app = live();
    let effects = app.update(Msg::Input(Command::Hotkey(target("alpha"))));
    let mask = published(&effects).unwrap();
    let effects = ack(&mut app, 2, mask);
    assert!(effects.contains(&Effect::Chime(Chime::Rise)));
    assert!(ack(&mut app, 2, mask).is_empty(), "the same revision again");
    assert!(app.take_redraw());
    assert!(
        ack(&mut app, 1, DesiredMask::ZERO).is_empty(),
        "an older revision"
    );
    assert!(!app.take_redraw());
    let effects = ack(&mut app, 3, mask);
    assert!(
        !effects.iter().any(|e| matches!(e, Effect::Chime(_))),
        "unchanged mask"
    );
    let effects = app.update(Msg::Input(Command::Hotkey(Target::Master)));
    assert_eq!(published(&effects), Some(DesiredMask::ZERO));
    let effects = ack(&mut app, 4, DesiredMask::ZERO);
    assert!(effects.contains(&Effect::Chime(Chime::Fall)));
    assert_eq!(app.status(id("alpha")), Status::Paused);
    assert_eq!(
        app.status(id("red")),
        Status::Off,
        "not selected or covered"
    );
}

#[test]
fn chimes_respect_the_setting() {
    let mut app = live();
    app.model.settings.sounds = false;
    let effects = app.update(Msg::Input(Command::Hotkey(target("alpha"))));
    let effects = ack(&mut app, 2, published(&effects).unwrap());
    assert!(!effects.iter().any(|e| matches!(e, Effect::Chime(_))));
}

#[test]
fn the_master_starts_from_start_paused_and_is_never_saved() {
    let mut settings = Settings::defaults(&profile());
    settings.start_paused = true;
    let mut app = App::new(
        profile(),
        Startup {
            settings,
            ..startup(Phase::Waiting)
        },
    );
    let effects = app.update(Msg::Runtime(RuntimeEvent::Phase(attached())));
    assert_eq!(published(&effects), Some(DesiredMask::ZERO));
    assert_eq!(app.model.master, Master::Paused);
    let effects = app.update(letter(b'P'));
    assert_ne!(published(&effects), Some(DesiredMask::ZERO));
    assert!(!effects.contains(&Effect::Save));
    // Game exit returns to Waiting; the next connection starts paused again.
    app.update(Msg::Runtime(RuntimeEvent::Phase(Phase::Waiting)));
    assert_eq!(app.status(Row::Master), Status::Inactive);
    let effects = app.update(Msg::Runtime(RuntimeEvent::Phase(attached())));
    assert_eq!(published(&effects), Some(DesiredMask::ZERO));
}

#[test]
fn pausing_before_connection_explains_itself() {
    let mut app = app(Phase::Waiting);
    let effects = app.update(Msg::Input(Command::Hotkey(Target::Master)));
    assert!(effects.is_empty());
    assert!(matches!(app.model.last, Some(super::Last::Message { .. })));
}

#[test]
fn quitting_while_attached_confirms_and_defaults_to_stay() {
    let mut app = live();
    assert!(app.update(letter(b'Q')).is_empty());
    assert!(matches!(
        app.model.modal,
        Some(Modal::Quit { quit: false, .. })
    ));
    app.update(press(ConsoleKey::Enter));
    assert!(app.model.modal.is_none(), "Enter on Stay stays");
    app.update(Msg::Input(Command::Interrupt));
    let effects = app.update(Msg::Input(Command::Interrupt));
    assert_eq!(
        effects,
        [Effect::Runtime(RuntimeCommand::Quit), Effect::Title]
    );
    assert!(app.update(letter(b'Q')).is_empty(), "already quitting");
    let mut waiting = app_waiting_quit();
    assert!(
        waiting
            .update(press(ConsoleKey::Escape))
            .contains(&Effect::Runtime(RuntimeCommand::Quit))
    );
}

fn app_waiting_quit() -> App<'static> {
    app(Phase::Waiting)
}

#[test]
fn escape_closes_a_dialog_first() {
    let mut app = live();
    app.update(letter(b'H'));
    assert!(matches!(app.model.modal, Some(Modal::Help { .. })));
    assert!(app.update(press(ConsoleKey::Escape)).is_empty());
    assert!(app.model.modal.is_none());
    app.update(letter(b'R'));
    let effects = app.update(press(ConsoleKey::Escape));
    assert_eq!(effects, [Effect::Bindings]);
    assert!(!app.capturing());
}

#[test]
fn capture_refuses_typing_keys_and_swaps_on_conflict() {
    let mut app = live();
    app.model.focus = id("alpha");
    app.update(letter(b'R'));
    app.update(letter(b'G'));
    assert!(matches!(
        &app.model.modal,
        Some(Modal::Capture {
            refusal: Some(_),
            ..
        })
    ));
    app.update(press(ConsoleKey::Up));
    assert!(matches!(
        &app.model.modal,
        Some(Modal::Capture {
            refusal: Some(_),
            ..
        })
    ));
    // Hotkeys never fire while capturing.
    assert!(
        app.update(Msg::Input(Command::Hotkey(target("beta"))))
            .is_empty()
    );
    let f2 = Chord::new(Modifiers::ALT, Key::function(2).unwrap());
    app.update(Msg::Input(Command::Press(Press {
        key: ConsoleKey::Key(Key::function(2).unwrap()),
        modifiers: Modifiers::ALT,
    })));
    assert!(matches!(app.model.modal, Some(Modal::Conflict { .. })));
    let effects = app.update(press(ConsoleKey::Enter));
    assert!(effects.contains(&Effect::Save) && effects.contains(&Effect::Bindings));
    let bindings = &app.settings().bindings;
    assert_eq!(bindings.get(target("alpha")).0, Some(f2));
    assert_eq!(
        bindings.get(target("beta")).0,
        Some(Chord::new(Modifiers::ALT, Key::function(1).unwrap()))
    );
    // Backspace removes a hotkey.
    app.update(letter(b'R'));
    app.update(press(ConsoleKey::Backspace));
    assert_eq!(app.settings().bindings.get(target("alpha")).0, None);
    // Options take no hotkey.
    app.model.focus = Row::Sounds;
    app.update(letter(b'R'));
    assert!(app.model.modal.is_none());
}

#[test]
fn input_waits_while_installing_or_restoring() {
    for phase in [Phase::Installing, Phase::Restoring] {
        let mut app = app(phase);
        let before = app.settings().clone();
        app.update(press(ConsoleKey::Key(Key::SPACE)));
        app.update(Msg::Input(Command::Hotkey(target("alpha"))));
        assert_eq!(*app.settings(), before);
    }
    let mut app = app(Phase::Restoring);
    assert!(app.update(Msg::Input(Command::Interrupt)).is_empty());
}

#[test]
fn the_settings_card_blocks_hotkeys_and_offers_its_commands() {
    let profile = profile();
    let error = Settings::parse(&profile, b"schema = 99\n").unwrap_err();
    let mut app = app(Phase::Settings(SettingsProblem::Invalid {
        path: PathBuf::from("settings.toml"),
        error,
    }));
    assert!(
        app.update(Msg::Input(Command::Hotkey(target("alpha"))))
            .is_empty()
    );
    assert_eq!(
        app.update(letter(b'D')),
        [Effect::Runtime(RuntimeCommand::ResetSettings)]
    );
    assert_eq!(
        app.update(press(ConsoleKey::Enter)),
        [Effect::Runtime(RuntimeCommand::Retry)]
    );
    assert!(
        app.update(letter(b'S')).is_empty(),
        "session-only needs a missing store"
    );
    let store = StoreError::Folder {
        path: PathBuf::from("dir"),
        source: std::io::Error::other("denied"),
    };
    app.update(Msg::Runtime(RuntimeEvent::Phase(Phase::Settings(
        SettingsProblem::store(&store),
    ))));
    assert_eq!(
        app.update(letter(b'S')),
        [Effect::Runtime(RuntimeCommand::SessionOnly)]
    );
    let effects = app.update(Msg::Loaded {
        settings: Settings::defaults(&profile),
        path: None,
    });
    assert_eq!(effects, [Effect::Bindings]);
    app.update(Msg::Runtime(RuntimeEvent::Phase(Phase::Waiting)));
    let effects = app.update(press(ConsoleKey::Key(Key::SPACE)));
    assert!(!effects.contains(&Effect::Save), "session-only never saves");
}

#[test]
fn failures_retry_only_without_left_hooks() {
    let failure = |unrestored| Failure {
        recovery: RecoveryAction::Retry,
        message: "boom".into(),
        unrestored,
    };
    let mut attached = live();
    attached.update(letter(b'H'));
    attached.update(Msg::Runtime(RuntimeEvent::Phase(Phase::Failed(failure(
        vec![],
    )))));
    assert!(attached.model.modal.is_none(), "a card closes dialogs");
    assert_eq!(
        attached.update(press(ConsoleKey::Enter)),
        [Effect::Runtime(RuntimeCommand::Retry)]
    );
    let mut app = app(Phase::Failed(failure(vec![
        edbfi_trainer::profile::SiteId(2),
    ])));
    assert!(app.update(press(ConsoleKey::Enter)).is_empty());
    assert!(
        app.update(Msg::Input(Command::Hotkey(target("alpha"))))
            .iter()
            .all(|e| !matches!(e, Effect::Runtime(RuntimeCommand::Publish(_))))
    );
}

#[test]
fn saves_report_failures_until_the_next_success() {
    let mut app = live();
    app.update(press(ConsoleKey::Key(Key::SPACE)));
    app.update(Msg::Saved(Err(StoreError::Write {
        path: PathBuf::from("settings.toml"),
        source: std::io::Error::other("disk full"),
    })));
    let notice = app.model.notice.clone().unwrap();
    assert!(notice.text.contains("disk full"));
    app.update(press(ConsoleKey::Down));
    assert!(app.model.notice.is_some(), "navigation keeps a failure");
    app.update(Msg::Saved(Ok(Saved::Written { external: None })));
    assert!(app.model.notice.is_none());
}

#[test]
fn a_lost_console_closes_dialogs_and_keeps_hotkeys() {
    let mut app = live();
    app.update(letter(b'R'));
    let effects = app.update(Msg::ConsoleLost);
    assert_eq!(effects, [Effect::Bindings]);
    assert!(app.plain() && app.model.modal.is_none());
    assert!(app.update(letter(b'R')).is_empty());
    assert!(published(&app.update(Msg::Input(Command::Hotkey(target("alpha"))))).is_some());
}

#[test]
fn focus_survives_resizing() {
    let mut app = live();
    app.update(press(ConsoleKey::Tab));
    let focus = app.model.focus;
    assert_eq!(focus, id("red"));
    for (width, height) in [(40, 12), (10, 3), (72, 24), (100, 30)] {
        app.update(Msg::Resize { width, height });
        assert_eq!(app.model.focus, focus);
    }
    // The title follows the phase and master only.
    let effects = app.update(letter(b'P'));
    assert!(effects.contains(&Effect::Title));
    assert!(app.title().ends_with("PAUSED"));
    assert!(!app.update(press(ConsoleKey::Down)).contains(&Effect::Title));
}
