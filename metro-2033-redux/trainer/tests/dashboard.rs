// SPDX-License-Identifier: AGPL-3.0-only
//! The Metro dashboard: every phase and dialog drawn at 100×30 and 40×12, each colour and glyph mode, and
//! property tests for reachability, clipping and redraws at any size.
//!
//! Snapshots hold every cell's symbol and its full style. Each text row ends in `|`; the line under it gives
//! each cell's style as a key from the legend, `.` being the terminal's default.
#![expect(
    clippy::unwrap_used,
    reason = "helpers shared by tests; an invalid profile is a test failure"
)]

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use edbfi_trainer::chord::{Chord, Key, Modifiers};
use edbfi_trainer::control::{Ack, ControlBit, ControlSet, DesiredMask};
use edbfi_trainer::input::{Command, ConsoleKey, Press};
use edbfi_trainer::journal::{EntryKind, Event, Journal};
use edbfi_trainer::lifecycle::{
    Failure, Finish, Phase, RuntimeCommand, RuntimeEvent, SettingsProblem, VerifyStage,
};
use edbfi_trainer::profile::{SiteId, ValidProfile};
use edbfi_trainer::recovery::RecoveryAction;
use edbfi_trainer::settings::{External, Saved, Settings, StoreError, Target};
use edbfi_tui::app::{App, Effect, Msg, Paths, Startup};
use edbfi_tui::plain::Report;
use edbfi_tui::theme::{Appearance, Colour, Glyphs};
use metro_2033_redux_trainer::profile::METRO;
use proptest::prelude::*;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

const WIDE: (u16, u16) = (100, 30);
const SMALL: (u16, u16) = (40, 12);

fn profile() -> ValidProfile<'static> {
    ValidProfile::new(&METRO).unwrap()
}

fn paths() -> Paths {
    Paths {
        settings: Some(PathBuf::from(
            r"C:\Users\Player\AppData\Roaming\edbfi\metro-2033-redux\trainer\settings.toml",
        )),
        log: Some(PathBuf::from(
            r"C:\Users\Player\AppData\Local\edbfi\metro-2033-redux\trainer\trainer.log",
        )),
    }
}

/// Defaults plus unlimited ammo, no reload for every weapon, and the Hellsing on its own.
fn settings() -> Settings {
    let profile = profile();
    let mut settings = Settings::defaults(&profile);
    for key in ["unlimited_ammo", "no_reload", "helsing"] {
        settings
            .selection
            .set(profile.toggle_id(key).unwrap(), true);
    }
    settings
}

fn app_with(phase: Phase, size: (u16, u16), appearance: Appearance) -> App<'static> {
    App::new(
        profile(),
        Startup {
            settings: settings(),
            paths: paths(),
            phase,
            appearance,
            size,
        },
    )
}

fn app(phase: Phase, size: (u16, u16)) -> App<'static> {
    app_with(phase, size, Appearance::default())
}

fn attached() -> Phase {
    Phase::Attached {
        attach_duration: Duration::from_millis(42),
    }
}

/// An app attached at `size` whose every publication was acknowledged.
fn live(size: (u16, u16), appearance: Appearance) -> App<'static> {
    let mut app = app_with(Phase::Waiting, size, appearance);
    run(&mut app, Msg::Runtime(RuntimeEvent::Phase(attached())));
    app
}

/// Sends `msg`, then acknowledges every publication it asked for, as a runtime would.
fn run(app: &mut App<'static>, msg: Msg) -> Vec<Effect> {
    let effects = app.update(msg);
    let mut all = effects.clone();
    for effect in effects {
        if let Effect::Runtime(RuntimeCommand::Publish(mask)) = effect {
            all.extend(ack(app, mask));
        }
    }
    all
}

fn ack(app: &mut App<'static>, mask: DesiredMask) -> Vec<Effect> {
    static REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let revision = REVISION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    app.update(Msg::Runtime(RuntimeEvent::Ack(Ack::simulated(
        revision, mask,
    ))))
}

fn press(key: ConsoleKey) -> Msg {
    Msg::Input(Command::Press(Press::plain(key)))
}

fn letter(byte: u8) -> Msg {
    press(ConsoleKey::Key(Key::letter(byte).unwrap()))
}

fn chord(modifiers: Modifiers, key: Key) -> Msg {
    Msg::Input(Command::Press(Press {
        key: ConsoleKey::Key(key),
        modifiers,
    }))
}

fn render(app: &App<'_>, (width, height): (u16, u16)) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    app.render(area, &mut buffer);
    buffer
}

/// Every cell's symbol, row by row, and a style key per cell with the legend.
fn cells(buffer: &Buffer) -> String {
    let mut styles: Vec<Style> = vec![Style::default()];
    let keys = ".abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut grid = String::new();
    let area = buffer.area;
    for y in area.top()..area.bottom() {
        let mut text = String::new();
        let mut style_line = String::new();
        let mut skip = 0;
        for x in area.left()..area.right() {
            let cell = &buffer[(x, y)];
            if skip > 0 {
                skip -= 1;
            } else {
                text.push_str(cell.symbol());
                skip = unicode_width(cell.symbol()).saturating_sub(1);
            }
            let style = Style::default()
                .fg(cell.fg)
                .bg(cell.bg)
                .add_modifier(cell.modifier);
            let index = styles.iter().position(|s| *s == style).unwrap_or_else(|| {
                styles.push(style);
                styles.len() - 1
            });
            style_line.push(keys.chars().nth(index).unwrap());
        }
        writeln!(grid, "{text}|").unwrap();
        writeln!(grid, "{}", style_line.trim_end_matches('.')).unwrap();
    }
    let mut out = format!("{}x{}\n", area.width, area.height);
    for (index, style) in styles.iter().enumerate() {
        writeln!(
            out,
            "{} fg={:?} bg={:?} mod={:?}",
            keys.chars().nth(index).unwrap(),
            style.fg.unwrap_or_default(),
            style.bg.unwrap_or_default(),
            style.add_modifier
        )
        .unwrap();
    }
    out + &grid
}

fn unicode_width(symbol: &str) -> usize {
    edbfi_tui::text::width(symbol)
}

fn snap(name: &str, app: &App<'_>, size: (u16, u16)) {
    insta::assert_snapshot!(name, cells(&render(app, size)));
}

/// Snapshots `app` at both reference sizes.
fn both(name: &str, make: impl Fn((u16, u16)) -> App<'static>) {
    snap(&format!("{name}_100x30"), &make(WIDE), WIDE);
    snap(&format!("{name}_40x12"), &make(SMALL), SMALL);
}

fn invalid_settings() -> Phase {
    let profile = profile();
    let error =
        Settings::parse(&profile, b"schema = 1\n\n[trainer]\nstart_paused = maybe\n").unwrap_err();
    Phase::Settings(SettingsProblem::Invalid {
        path: paths().settings.unwrap(),
        error,
    })
}

// ----- phases -----

#[test]
fn waiting() {
    both("waiting", |size| app(Phase::Waiting, size));
}

#[test]
fn verifying() {
    both("verifying", |size| {
        app(
            Phase::Verifying {
                stage: VerifyStage::Unpacking,
            },
            size,
        )
    });
}

#[test]
fn installing() {
    both("installing", |size| app(Phase::Installing, size));
}

#[test]
fn attached_active() {
    both("attached_active", |size| {
        let mut app = live(size, Appearance::default());
        run(&mut app, letter(b'X'));
        app
    });
}

#[test]
fn attached_paused() {
    both("attached_paused", |size| {
        let mut app = live(size, Appearance::default());
        run(&mut app, letter(b'P'));
        app
    });
}

#[test]
fn attached_pending() {
    // Focus no reload, select it again and do not acknowledge: rows keep their acknowledged state.
    both("attached_pending", |size| {
        let mut app = live(size, Appearance::default());
        for _ in 0..5 {
            run(&mut app, press(ConsoleKey::Down));
        }
        app.update(press(ConsoleKey::Key(Key::SPACE)));
        app
    });
}

#[test]
fn restoring() {
    both("restoring", |size| app(Phase::Restoring, size));
}

#[test]
fn finished() {
    both("finished", |size| {
        app(Phase::Finished(Finish::Restored), size)
    });
}

#[test]
fn settings_invalid() {
    both("settings_invalid", |size| app(invalid_settings(), size));
}

#[test]
fn settings_unavailable() {
    both("settings_unavailable", |size| {
        let error = StoreError::Create {
            path: paths().settings.unwrap(),
            source: std::io::Error::other("Access is denied. (os error 5)"),
        };
        app(Phase::Settings(SettingsProblem::store(&error)), size)
    });
}

#[test]
fn failed() {
    both("failed", |size| {
        app(
            Phase::Failed(Failure {
                recovery: RecoveryAction::CloseDuplicates,
                message: "Two copies of metro.exe are running (PIDs 4120 and 7788).".into(),
                unrestored: Vec::new(),
            }),
            size,
        )
    });
}

#[test]
fn restore_incomplete() {
    both("restore_incomplete", |size| {
        app(
            Phase::Failed(Failure {
                recovery: RecoveryAction::RestartGame,
                message:
                    "Restoring the game's code failed: WriteProcessMemory was denied (OS error 5)."
                        .into(),
                unrestored: vec![SiteId(3), SiteId(7)],
            }),
            size,
        )
    });
}

// ----- dialogs and notices -----

#[test]
fn help() {
    both("help", |size| {
        let mut app = live(size, Appearance::default());
        app.update(Msg::Input(Command::Press(Press {
            key: ConsoleKey::Char('?'),
            modifiers: Modifiers::SHIFT,
        })));
        app
    });
}

#[test]
fn capture_refused() {
    both("capture_refused", |size| {
        let mut app = live(size, Appearance::default());
        app.update(letter(b'R'));
        app.update(letter(b'G'));
        app
    });
}

#[test]
fn conflict() {
    both("conflict", |size| {
        let mut app = live(size, Appearance::default());
        app.update(letter(b'R'));
        app.update(chord(Modifiers::ALT, Key::function(2).unwrap()));
        app
    });
}

#[test]
fn quit_after_interrupt() {
    both("quit_after_interrupt", |size| {
        let mut app = live(size, Appearance::default());
        app.update(Msg::Input(Command::Interrupt));
        app
    });
}

#[test]
fn external_edit_kept() {
    both("external_edit_kept", |size| {
        let mut app = live(size, Appearance::default());
        run(&mut app, press(ConsoleKey::Key(Key::SPACE)));
        app.update(Msg::Saved(Ok(Saved::Written {
            external: Some(External::Kept(PathBuf::from(
                r"C:\Users\Player\AppData\Roaming\edbfi\metro-2033-redux\trainer\settings.external-1.toml",
            ))),
        })));
        app
    });
}

#[test]
fn save_failed() {
    both("save_failed", |size| {
        let mut app = live(size, Appearance::default());
        run(&mut app, press(ConsoleKey::Key(Key::SPACE)));
        app.update(Msg::Saved(Err(StoreError::Write {
            path: paths().settings.unwrap(),
            source: std::io::Error::other("The disk is full. (os error 112)"),
        })));
        app
    });
}

#[test]
fn journal_warning_and_hostile_text() {
    both("journal_warning", |size| {
        let mut app = live(size, Appearance::default());
        let mut journal = Journal::memory();
        journal.record(Event::warning(
            EntryKind::Session,
            "Overlay \u{1b}[31mhooked\u{1b}[0m \u{202e}exe.metro: Alt+F2 may not reach the trainer",
        ));
        let entry = journal.activity().last().unwrap().clone();
        app.update(Msg::Runtime(RuntimeEvent::Journal(entry)));
        app
    });
}

#[test]
fn session_only_with_unicode_paths() {
    both("session_only", |size| {
        let mut app = App::new(
            profile(),
            Startup {
                settings: settings(),
                paths: Paths {
                    settings: None,
                    log: Some(PathBuf::from(
                        "C:\\Users\\Jérôme\\Документы\\edbfi\\\u{1b}]0;pwned\u{7}\\trainer.log",
                    )),
                },
                phase: Phase::Waiting,
                appearance: Appearance::default(),
                size,
            },
        );
        app.update(press(ConsoleKey::Key(Key::SPACE)));
        app
    });
}

// ----- other sizes and modes -----

#[test]
fn other_sizes() {
    for size in [(99, 20), (72, 24), (30, 3), (16, 1)] {
        let app = live(size, Appearance::default());
        snap(&format!("attached_{}x{}", size.0, size.1), &app, size);
    }
    let mut app = app(Phase::Waiting, (72, 24));
    app.update(press(ConsoleKey::Tab));
    for _ in 0..30 {
        app.update(press(ConsoleKey::Down));
    }
    snap("weapons_tab_scrolled_72x24", &app, (72, 24));
}

#[test]
fn colour_and_glyph_modes() {
    for (name, colour, glyphs) in [
        ("truecolour", Colour::TrueColour, Glyphs::Unicode),
        ("mono", Colour::Mono, Glyphs::Unicode),
        ("ascii", Colour::Ansi16, Glyphs::Ascii),
    ] {
        let appearance = Appearance { colour, glyphs };
        let mut app = live(WIDE, appearance);
        run(&mut app, letter(b'P'));
        run(&mut app, letter(b'P'));
        snap(&format!("mode_{name}_100x30"), &app, WIDE);
    }
}

#[test]
fn plain_report_lists_everything_then_changes() {
    let mut app = app(Phase::Waiting, WIDE);
    let mut report = Report::new();
    let first = report.lines(&app).join("\n");
    assert!(report.lines(&app).is_empty(), "nothing changed");
    run(&mut app, Msg::Runtime(RuntimeEvent::Phase(attached())));
    run(&mut app, letter(b'P'));
    let second = report.lines(&app).join("\n");
    app.update(Msg::Runtime(RuntimeEvent::Phase(invalid_settings())));
    let third = report.lines(&app).join("\n");
    insta::assert_snapshot!("plain", format!("{first}\n---\n{second}\n---\n{third}"));
    assert!(first.lines().count() > 8 + 24, "every cheat and weapon");
    assert!(
        format!("{first}{second}{third}")
            .bytes()
            .all(|b| b == b'\n' || (0x20..0x7f).contains(&b))
    );
}

// ----- behaviour -----

#[test]
fn hotkeys_toggle_and_the_master_pauses() {
    let profile = profile();
    let mut app = live(WIDE, Appearance::default());
    let god = profile.toggle_id("god_mode").unwrap();
    let effects = run(&mut app, Msg::Input(Command::Hotkey(Target::Toggle(god))));
    assert!(effects.contains(&Effect::Save));
    assert!(effects.iter().any(|e| matches!(e, Effect::Chime(_))));
    assert!(app.settings().selection.contains(god));
    let effects = run(&mut app, Msg::Input(Command::Hotkey(Target::Master)));
    assert!(
        effects.contains(&Effect::Runtime(RuntimeCommand::Publish(DesiredMask::ZERO))),
        "pausing publishes zero"
    );
    assert!(
        !effects.contains(&Effect::Save),
        "pause state never persists"
    );
}

#[test]
fn rebinding_swaps_on_conflict() {
    let profile = profile();
    let mut app = live(WIDE, Appearance::default());
    let ammo = Target::Toggle(profile.toggle_id("unlimited_ammo").unwrap());
    let throwables = Target::Toggle(profile.toggle_id("unlimited_throwables").unwrap());
    let f1 = Chord::new(Modifiers::ALT, Key::function(1).unwrap());
    let f2 = Chord::new(Modifiers::ALT, Key::function(2).unwrap());
    app.update(letter(b'R'));
    assert!(app.capturing());
    app.update(chord(Modifiers::ALT, Key::function(2).unwrap()));
    let effects = app.update(press(ConsoleKey::Enter));
    assert!(effects.contains(&Effect::Save) && effects.contains(&Effect::Bindings));
    assert!(!app.capturing());
    assert_eq!(app.settings().bindings.get(ammo).0, Some(f2));
    assert_eq!(app.settings().bindings.get(throwables).0, Some(f1));
    // Num5 alone is allowed.
    app.update(letter(b'R'));
    app.update(chord(Modifiers::NONE, Key::numpad(5).unwrap()));
    assert_eq!(
        app.settings().bindings.get(ammo).0,
        Some(Chord::new(Modifiers::NONE, Key::numpad(5).unwrap()))
    );
}

// ----- properties -----

/// The labels of every row reached by Tab and Down, checking each focused row is drawn.
fn walk(size: (u16, u16)) -> usize {
    let mut app = app(Phase::Waiting, size);
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..8 {
        app.update(press(ConsoleKey::Key(Key::HOME)));
        for _ in 0..40 {
            let buffer = render(&app, size);
            let focus = (0..size.1)
                .find(|&y| (0..size.0).any(|x| matches!(buffer[(x, y)].symbol(), "►")))
                .map(|y| {
                    (0..size.0)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                });
            let line = focus.unwrap_or_else(|| panic!("no focused row drawn at {size:?}"));
            seen.insert(line.split('►').nth(1).unwrap().trim().to_owned());
            app.update(press(ConsoleKey::Down));
        }
        app.update(press(ConsoleKey::Tab));
    }
    seen.len()
}

#[test]
fn every_row_is_reachable_and_drawn_at_supported_sizes() {
    // 8 cheats, 24 weapons, the master, 1 option, chimes and start paused.
    for size in [(100, 30), (99, 30), (99, 20), (72, 24), (49, 16), (40, 12)] {
        assert_eq!(walk(size), 36, "{size:?}");
    }
}

/// A message a property test can generate; [`Msg`] holds errors that cannot be cloned.
#[derive(Clone, Debug)]
enum Step {
    Press(Press),
    Resize(u16, u16),
    Master,
    Attach(bool),
    Ack(u64, u32),
}

impl Step {
    fn msg(self) -> Msg {
        match self {
            Self::Press(press) => Msg::Input(Command::Press(press)),
            Self::Resize(width, height) => Msg::Resize { width, height },
            Self::Master => Msg::Input(Command::Hotkey(Target::Master)),
            Self::Attach(true) => Msg::Runtime(RuntimeEvent::Phase(attached())),
            Self::Attach(false) => Msg::Runtime(RuntimeEvent::Phase(Phase::Waiting)),
            Self::Ack(revision, word) => {
                let bits = (0..32)
                    .filter(|bit| word & 1 << bit != 0)
                    .fold(ControlSet::EMPTY, |set, bit| set.with(ControlBit::new(bit)));
                Msg::Runtime(RuntimeEvent::Ack(Ack::simulated(
                    revision,
                    DesiredMask::new(bits),
                )))
            }
        }
    }
}

fn any_step() -> impl Strategy<Value = Step> {
    let keys = prop_oneof![
        Just(ConsoleKey::Up),
        Just(ConsoleKey::Down),
        Just(ConsoleKey::Left),
        Just(ConsoleKey::Right),
        Just(ConsoleKey::Tab),
        Just(ConsoleKey::Enter),
        Just(ConsoleKey::Escape),
        Just(ConsoleKey::Backspace),
        Just(ConsoleKey::Char('?')),
        Just(ConsoleKey::Key(Key::SPACE)),
        Just(ConsoleKey::Key(Key::PAGE_DOWN)),
        Just(ConsoleKey::Key(Key::END)),
        Just(ConsoleKey::Key(Key::letter(b'R').unwrap())),
        Just(ConsoleKey::Key(Key::letter(b'P').unwrap())),
        Just(ConsoleKey::Key(Key::function(2).unwrap())),
    ];
    let modifiers = prop_oneof![
        Just(Modifiers::NONE),
        Just(Modifiers::ALT),
        Just(Modifiers::SHIFT)
    ];
    prop_oneof![
        6 => (keys, modifiers).prop_map(|(key, modifiers)| Step::Press(Press { key, modifiers })),
        1 => (0u16..140, 0u16..45).prop_map(|(width, height)| Step::Resize(width, height)),
        1 => Just(Step::Master),
        1 => any::<bool>().prop_map(Step::Attach),
        1 => (0u64..8, any::<u32>()).prop_map(|(revision, word)| Step::Ack(revision, word)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// No size or message sequence panics; every drawn frame that differs was reported as needing a redraw.
    #[test]
    fn redraws_are_never_missed(steps in proptest::collection::vec(any_step(), 1..40)) {
        let mut app = app(Phase::Waiting, WIDE);
        let mut size = WIDE;
        app.take_redraw();
        let mut before = render(&app, size);
        for step in steps {
            if let Step::Resize(width, height) = step {
                size = (width, height);
            }
            app.update(step.msg());
            let after = render(&app, size);
            let redraw = app.take_redraw();
            if after != before {
                prop_assert!(redraw, "a visible change was not reported");
            }
            before = after;
        }
    }

    /// A message that changes nothing visible never asks for a redraw.
    #[test]
    fn idle_and_repeated_messages_do_not_redraw(width in 0u16..140, height in 0u16..45) {
        let mut app = live((width, height), Appearance::default());
        app.take_redraw();
        app.update(Msg::Resize { width, height });
        prop_assert!(!app.take_redraw(), "same size");
        let mask = profile().effective(&app.desired());
        app.update(Msg::Runtime(RuntimeEvent::Ack(Ack::simulated(0, mask))));
        prop_assert!(!app.take_redraw(), "a stale acknowledgement");
        app.update(press(ConsoleKey::Key(Key::HOME)));
        app.take_redraw();
        app.update(press(ConsoleKey::Key(Key::HOME)));
        prop_assert!(!app.take_redraw(), "moving past the top");
    }
}
