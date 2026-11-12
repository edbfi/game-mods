// SPDX-License-Identifier: AGPL-3.0-or-later
//! The arbiter against a simulated keyboard: every console behaviour the trainer promises, then generated
//! traces checking that one physical press acts at most once.

use proptest::prelude::*;

use super::*;
use crate::profile::ValidProfile;
use crate::profile::synthetic::SYNTHETIC;

const F1: u8 = 0x70;
const F2: u8 = 0x71;
const F8: u8 = 0x77;
const SHIFT: u8 = 0x10;
const CTRL: u8 = 0x11;
const ALT: u8 = 0x12;
const LWIN: u8 = 0x5B;
const KEY_A: u8 = 0x41;
const KEY_C: u8 = 0x43;
const DOWN: u8 = 0x28;
const GAME: u32 = 4242;
const OTHER: u32 = 7;

/// The physical keyboard and windows.
#[derive(Clone, Debug)]
struct World {
    down: [bool; 256],
    foreground: Option<u32>,
    console: ConsoleFocus,
}

impl Probe for World {
    fn down(&self, vk: u8) -> bool {
        self.down[usize::from(vk)]
    }

    fn modifiers(&self) -> Option<Modifiers> {
        if self.down[usize::from(LWIN)] {
            return None;
        }
        let mut mods = Modifiers::NONE;
        for (vk, modifier) in [
            (CTRL, Modifiers::CTRL),
            (ALT, Modifiers::ALT),
            (SHIFT, Modifiers::SHIFT),
        ] {
            if self.down[usize::from(vk)] {
                mods = mods.union(modifier);
            }
        }
        Some(mods)
    }

    fn foreground(&self) -> Option<u32> {
        self.foreground
    }

    fn console(&self) -> ConsoleFocus {
        self.console
    }
}

fn profile() -> ValidProfile<'static> {
    ValidProfile::new(&SYNTHETIC).unwrap()
}

fn alpha() -> Command {
    Command::Hotkey(Target::Toggle(profile().toggle_id("alpha").unwrap()))
}

fn beta() -> Command {
    Command::Hotkey(Target::Toggle(profile().toggle_id("beta").unwrap()))
}

struct Rig {
    arbiter: Arbiter,
    world: World,
}

impl Rig {
    /// The synthetic profile's default bindings (Alt+F1 alpha, Alt+F2 beta, Alt+1 all colours, Alt+F8 master),
    /// attached to a game that is behind a Windows Terminal tab.
    fn new() -> Self {
        let mut rig = Self {
            arbiter: Arbiter::new(&Bindings::defaults(&profile())),
            world: World {
                down: [false; 256],
                foreground: Some(OTHER),
                console: ConsoleFocus::Hidden,
            },
        };
        rig.arbiter.set_game(Some(GAME));
        rig.reopen();
        rig
    }

    fn reopen(&mut self) {
        let epoch = self.arbiter.reopen().unwrap();
        let flush = self.arbiter.barrier(epoch).unwrap();
        self.arbiter.open(flush, &self.world);
        assert!(self.arbiter.is_open());
    }

    fn set(&mut self, keys: &[u8], down: bool) {
        for &vk in keys {
            self.world.down[usize::from(vk)] = down;
        }
    }

    /// Presses `keys` and reports the wake.
    fn press(&mut self, keys: &[u8]) -> Vec<Command> {
        self.set(keys, true);
        self.wake()
    }

    /// Releases `keys` and reports the wake.
    fn release(&mut self, keys: &[u8]) -> Vec<Command> {
        self.set(keys, false);
        self.wake()
    }

    fn wake(&mut self) -> Vec<Command> {
        self.arbiter.physical(&self.world)
    }

    fn batch(&mut self, records: &[Record]) -> Vec<Command> {
        self.read(records, true)
    }

    fn read(&mut self, records: &[Record], complete: bool) -> Vec<Command> {
        let batch = Batch {
            records: records.to_vec(),
            complete,
            resized: false,
        };
        self.arbiter.console(&batch, &self.world)
    }

    fn game_front(&mut self) {
        self.world.foreground = Some(GAME);
        self.arbiter.foreground(&self.world);
    }

    /// A classic console window comes to the front, and the trainer reads the empty console once.
    fn classic_front(&mut self) {
        self.world.foreground = Some(OTHER);
        self.world.console = ConsoleFocus::Front;
        self.arbiter.foreground(&self.world);
        assert_eq!(self.batch(&[]), []);
    }
}

fn key(vk: u8, down: bool, modifiers: Modifiers) -> Record {
    Record::Key(KeyRecord {
        vk: u16::from(vk),
        down,
        modifiers,
        text: None,
    })
}

/// Only the hotkeys among `commands`.
fn hotkeys(commands: Vec<Command>) -> Vec<Command> {
    commands
        .into_iter()
        .filter(|command| matches!(command, Command::Hotkey(_)))
        .collect()
}

fn alt_down(vk: u8) -> Record {
    key(vk, true, Modifiers::ALT)
}

fn alt_up(vk: u8) -> Record {
    key(vk, false, Modifiers::ALT)
}

// ----- the game path -----

#[test]
fn a_fresh_press_with_the_game_in_front_acts_once() {
    let mut rig = Rig::new();
    rig.game_front();
    assert_eq!(rig.press(&[ALT, F1]), [alpha()]);
    // Autorepeat wakes the trainer again while the key is held.
    assert_eq!(rig.wake(), []);
    assert_eq!(rig.wake(), []);
    assert_eq!(rig.release(&[F1]), []);
    assert_eq!(rig.press(&[F1]), [alpha()]);
}

#[test]
fn the_game_path_needs_the_game_in_front() {
    let mut rig = Rig::new();
    rig.world.console = ConsoleFocus::Behind;
    assert_eq!(rig.press(&[ALT, F1]), []);
    rig.release(&[ALT, F1]);
    rig.arbiter.set_game(None);
    rig.game_front();
    assert_eq!(rig.press(&[ALT, F1]), []);
}

#[test]
fn modifiers_must_match_exactly_on_the_game_path() {
    let mut rig = Rig::new();
    rig.game_front();
    assert_eq!(rig.press(&[F1]), [], "no modifier");
    // Adding the modifier while the key is held is not a fresh press of the key.
    assert_eq!(rig.press(&[ALT]), []);
    rig.release(&[F1]);
    assert_eq!(rig.press(&[SHIFT, F1]), [], "one modifier too many");
    rig.release(&[SHIFT, F1]);
    assert_eq!(rig.press(&[LWIN, F1]), [], "a Windows key held");
    rig.release(&[LWIN, F1]);
    assert_eq!(rig.press(&[F1]), [alpha()]);
}

#[test]
fn releasing_a_modifier_never_rearms_a_held_key() {
    let mut rig = Rig::new();
    rig.game_front();
    assert_eq!(rig.press(&[ALT, F1]), [alpha()]);
    assert_eq!(rig.release(&[ALT]), []);
    assert_eq!(rig.press(&[ALT]), []);
    assert_eq!(rig.release(&[F1]), []);
    assert_eq!(rig.press(&[F1]), [alpha()]);
}

#[test]
fn simultaneous_bindings_each_act_once() {
    let mut rig = Rig::new();
    rig.game_front();
    rig.set(&[ALT, F1, F2], true);
    assert_eq!(rig.wake(), [alpha(), beta()]);
    assert_eq!(rig.wake(), []);
    rig.release(&[F1, F2]);
    rig.world.foreground = Some(OTHER);
    rig.arbiter.foreground(&rig.world);
    rig.set(&[F1, F2], true);
    assert_eq!(rig.batch(&[alt_down(F1), alt_down(F2)]), [alpha(), beta()]);
    assert_eq!(rig.batch(&[alt_down(F1), alt_down(F2)]), []);
}

#[test]
fn console_records_read_with_the_game_in_front_do_nothing() {
    let mut rig = Rig::new();
    rig.game_front();
    rig.set(&[ALT, F1], true);
    let records = [alt_down(F1), key(0x20, true, Modifiers::NONE)];
    assert_eq!(rig.batch(&records), []);
    assert_eq!(rig.wake(), []);
}

#[test]
fn a_release_reported_late_is_confirmed_by_the_next_wake() {
    let mut rig = Rig::new();
    rig.game_front();
    assert_eq!(rig.press(&[ALT, F1]), [alpha()]);
    // The release itself woke nobody; the next keyboard event does.
    rig.set(&[F1], false);
    assert_eq!(rig.press(&[KEY_A]), []);
    assert_eq!(rig.press(&[F1]), [alpha()]);
}

#[test]
fn removing_a_keyboard_primes_held_keys() {
    let mut rig = Rig::new();
    rig.set(&[ALT, F1], true);
    rig.arbiter.devices(&rig.world);
    rig.game_front();
    assert_eq!(rig.wake(), []);
}

// ----- the console path: a Windows Terminal tab -----

#[test]
fn a_pseudo_console_record_acts_while_the_key_is_held() {
    let mut rig = Rig::new();
    rig.set(&[ALT, F1], true);
    assert_eq!(rig.batch(&[alt_down(F1)]), [alpha()]);
    // Autorepeat records, and the wake behind them, change nothing.
    assert_eq!(rig.batch(&[alt_down(F1)]), []);
    assert_eq!(rig.wake(), []);
}

#[test]
fn a_delayed_record_meets_an_earlier_physical_press() {
    let mut rig = Rig::new();
    assert_eq!(rig.press(&[ALT, F1]), []);
    // Empty reads never use up the waiting press.
    for _ in 0..3 {
        assert_eq!(rig.batch(&[]), []);
        assert_eq!(rig.wake(), []);
    }
    assert_eq!(rig.batch(&[alt_down(F1)]), [alpha()]);
    assert_eq!(rig.batch(&[alt_down(F1)]), [], "autorepeat");
}

#[test]
fn a_focus_record_cancels_a_waiting_press_for_the_rest_of_its_hold() {
    let mut rig = Rig::new();
    rig.press(&[ALT, F1]);
    assert_eq!(rig.batch(&[alt_down(F1), Record::Focus]), []);
    assert_eq!(rig.batch(&[alt_down(F1)]), []);
    rig.release(&[F1]);
    rig.press(&[F1]);
    assert_eq!(rig.batch(&[Record::Focus, alt_down(F1)]), []);
}

#[test]
fn a_queued_release_cancels_the_press_before_it() {
    let mut rig = Rig::new();
    rig.press(&[ALT, F1]);
    assert_eq!(rig.batch(&[alt_down(F1), alt_up(F1)]), []);
}

#[test]
fn a_modifier_change_cancels_a_waiting_press() {
    let mut rig = Rig::new();
    rig.press(&[ALT, F1]);
    let shifted = Modifiers::ALT.union(Modifiers::SHIFT);
    rig.set(&[SHIFT], true);
    assert_eq!(hotkeys(rig.batch(&[key(F1, true, shifted)])), []);
    rig.set(&[SHIFT], false);
    assert_eq!(rig.batch(&[alt_down(F1)]), []);
}

#[test]
fn a_record_arriving_after_the_physical_release_cannot_act() {
    let mut rig = Rig::new();
    rig.press(&[ALT, F1]);
    rig.release(&[F1]);
    assert_eq!(rig.batch(&[alt_down(F1), alt_up(F1)]), []);
    // Nor does it wait for the next press.
    assert_eq!(rig.press(&[F1]), []);
    assert_eq!(
        rig.batch(&[alt_down(F1)]),
        [alpha()],
        "the next press is fresh"
    );
}

#[test]
fn a_partial_batch_cannot_hide_a_queued_focus_loss() {
    let mut rig = Rig::new();
    rig.press(&[ALT, F1]);
    let mut records = vec![alt_down(F1)];
    records.extend([Record::Focus; 63]);
    assert_eq!(rig.read(&records, false), []);
    assert_eq!(rig.batch(&[Record::Focus]), []);
    assert_eq!(rig.batch(&[alt_down(F1)]), [], "draining cannot revive it");
}

#[test]
fn a_partial_batch_authorises_nothing_even_without_a_focus_record() {
    let mut rig = Rig::new();
    rig.set(&[ALT, F1], true);
    assert_eq!(rig.read(&[alt_down(F1)], false), []);
    assert_eq!(rig.batch(&[alt_down(F1)]), []);
}

#[test]
fn a_background_tap_is_rejected() {
    let mut rig = Rig::new();
    rig.world.console = ConsoleFocus::Behind;
    assert_eq!(rig.batch(&[alt_down(F1), alt_up(F1)]), []);
}

#[test]
fn unrelated_typing_does_not_block_a_later_chord() {
    let mut rig = Rig::new();
    rig.set(&[ALT, F1], true);
    let typed = [
        key(KEY_A, true, Modifiers::NONE),
        key(KEY_A, false, Modifiers::NONE),
        alt_down(F1),
    ];
    assert_eq!(
        rig.batch(&typed),
        [
            Command::Press(Press::plain(ConsoleKey::Key(Key::letter(b'A').unwrap()))),
            alpha()
        ]
    );
}

#[test]
fn the_console_and_the_game_share_one_hold() {
    let mut rig = Rig::new();
    // Console first, then the game takes the focus with the key still down.
    rig.set(&[ALT, F1], true);
    assert_eq!(rig.batch(&[alt_down(F1)]), [alpha()]);
    rig.game_front();
    assert_eq!(rig.wake(), []);
    rig.release(&[F1]);
    // The game first, then the console.
    assert_eq!(rig.press(&[F1]), [alpha()]);
    rig.world.foreground = Some(OTHER);
    rig.arbiter.foreground(&rig.world);
    assert_eq!(rig.batch(&[alt_down(F1)]), []);
    // Even without a focus notice in between.
    rig.release(&[F1]);
    rig.game_front();
    assert_eq!(rig.press(&[F1]), [alpha()]);
    rig.world.foreground = Some(OTHER);
    assert_eq!(rig.batch(&[alt_down(F1)]), []);
}

// ----- the console path: a classic console window -----

#[test]
fn a_classic_window_accepts_a_short_tap_once_established() {
    let mut rig = Rig::new();
    rig.world.console = ConsoleFocus::Front;
    rig.arbiter.foreground(&rig.world);
    // Records read on the way in may predate the focus change.
    assert_eq!(rig.batch(&[alt_down(F1), alt_up(F1)]), []);
    assert_eq!(rig.batch(&[alt_down(F1), alt_up(F1)]), [alpha()]);
    assert_eq!(rig.batch(&[]), [], "acted once");
}

#[test]
fn an_empty_read_after_the_focus_notice_establishes_the_window() {
    let mut rig = Rig::new();
    rig.classic_front();
    assert_eq!(rig.batch(&[alt_down(F1), alt_up(F1)]), [alpha()]);
}

#[test]
fn a_completed_classic_tap_survives_a_later_modifier_release() {
    let mut rig = Rig::new();
    rig.classic_front();
    let records = [alt_down(F1), alt_up(F1), key(ALT, false, Modifiers::NONE)];
    assert_eq!(rig.batch(&records), [alpha()]);
}

#[test]
fn a_modifier_change_cancels_an_unfinished_classic_chord() {
    let mut rig = Rig::new();
    rig.classic_front();
    let shifted = Modifiers::ALT.union(Modifiers::SHIFT);
    let records = [
        alt_down(F1),
        key(SHIFT, true, shifted),
        key(F1, false, shifted),
    ];
    assert_eq!(rig.batch(&records), []);
}

#[test]
fn a_classic_hold_acts_once_and_never_again_in_the_game() {
    let mut rig = Rig::new();
    rig.classic_front();
    rig.set(&[ALT, F1], true);
    assert_eq!(rig.batch(&[alt_down(F1), alt_down(F1)]), [alpha()]);
    assert_eq!(rig.batch(&[alt_down(F1)]), [], "autorepeat");
    rig.game_front();
    assert_eq!(rig.wake(), []);
}

#[test]
fn classic_paired_bindings_each_act_and_modifiers_match_exactly() {
    let mut rig = Rig::new();
    rig.classic_front();
    let records = [alt_down(F1), alt_up(F1), alt_down(F2), alt_up(F2)];
    assert_eq!(rig.batch(&records), [alpha(), beta()]);
    let shifted = Modifiers::ALT.union(Modifiers::SHIFT);
    let records = [key(F1, true, shifted), key(F1, false, shifted)];
    assert_eq!(hotkeys(rig.batch(&records)), []);
}

#[test]
fn a_focus_record_cancels_a_classic_tap_around_it() {
    let mut rig = Rig::new();
    rig.classic_front();
    assert_eq!(rig.batch(&[alt_down(F1), Record::Focus, alt_up(F1)]), []);
    assert_eq!(rig.batch(&[alt_down(F1), alt_up(F1)]), [alpha()]);
}

#[test]
fn a_focus_record_read_on_the_way_in_establishes_the_window() {
    let mut rig = Rig::new();
    rig.world.console = ConsoleFocus::Front;
    rig.arbiter.foreground(&rig.world);
    // The console queues a focus record as it gains the focus; the next tap is the user's.
    assert_eq!(rig.batch(&[Record::Focus]), []);
    assert_eq!(rig.batch(&[alt_down(F1), alt_up(F1)]), [alpha()]);
}

#[test]
fn an_autorepeat_record_read_after_the_release_does_not_act_again() {
    let mut rig = Rig::new();
    rig.classic_front();
    rig.set(&[ALT, F1], true);
    assert_eq!(rig.batch(&[alt_down(F1)]), [alpha()]);
    rig.release(&[F1]);
    assert_eq!(rig.batch(&[alt_down(F1), alt_up(F1)]), []);
    // Nor after the keyboard was primed instead.
    rig.set(&[F1], true);
    assert_eq!(rig.batch(&[alt_down(F1)]), [alpha()]);
    rig.set(&[F1], false);
    rig.arbiter.devices(&rig.world);
    assert_eq!(rig.batch(&[alt_down(F1)]), []);
}

#[test]
fn a_dashboard_autorepeat_read_after_the_release_does_nothing() {
    let mut rig = Rig::new();
    let space = Command::Press(Press::plain(ConsoleKey::Key(Key::SPACE)));
    rig.set(&[0x20], true);
    assert_eq!(rig.batch(&[key(0x20, true, Modifiers::NONE)]), [space]);
    rig.release(&[0x20]);
    let late = [
        key(0x20, true, Modifiers::NONE),
        key(0x20, false, Modifiers::NONE),
    ];
    assert_eq!(rig.batch(&late), []);
    assert_eq!(
        rig.batch(&[key(0x20, true, Modifiers::NONE)]),
        [space],
        "a real press"
    );
}

#[test]
fn a_press_and_a_repress_read_together_each_count() {
    let mut rig = Rig::new();
    rig.classic_front();
    rig.set(&[ALT, F1], true);
    let records = [alt_down(F1), alt_up(F1), alt_down(F1)];
    assert_eq!(rig.batch(&records), [alpha(), alpha()]);
    // In a pseudo console the tap is lost, but the held press still acts.
    rig.release(&[F1]);
    assert_eq!(rig.batch(&[alt_up(F1)]), []);
    rig.world.console = ConsoleFocus::Hidden;
    rig.set(&[F1], true);
    assert_eq!(rig.batch(&records), [alpha()]);
}

#[test]
fn a_windows_key_blocks_console_hotkeys() {
    let mut rig = Rig::new();
    rig.classic_front();
    rig.set(&[LWIN], true);
    assert_eq!(rig.batch(&[alt_down(F1), alt_up(F1)]), []);
}

// ----- dashboard keys -----

#[test]
fn keys_that_are_not_hotkeys_become_presses() {
    let mut rig = Rig::new();
    let press = |key, modifiers| Command::Press(Press { key, modifiers });
    let question = Record::Key(KeyRecord {
        vk: 0xBF,
        down: true,
        modifiers: Modifiers::SHIFT,
        text: Some('?'),
    });
    let records = [
        key(DOWN, true, Modifiers::NONE),
        key(DOWN, true, Modifiers::NONE),
        key(0x20, true, Modifiers::NONE),
        key(0x20, true, Modifiers::NONE),
        key(CTRL, true, Modifiers::CTRL),
        key(KEY_C, true, Modifiers::CTRL),
        key(0x0D, true, Modifiers::ALT),
        question,
        key(0xAD, true, Modifiers::NONE),
    ];
    assert_eq!(
        rig.batch(&records),
        [
            press(ConsoleKey::Down, Modifiers::NONE),
            press(ConsoleKey::Down, Modifiers::NONE),
            press(ConsoleKey::Key(Key::SPACE), Modifiers::NONE),
            Command::Interrupt,
            press(ConsoleKey::Enter, Modifiers::ALT),
            press(ConsoleKey::Char('?'), Modifiers::SHIFT),
        ]
    );
}

#[test]
fn a_press_that_acted_in_the_game_is_not_a_dashboard_key_later() {
    let mut rig = Rig::new();
    rig.game_front();
    assert_eq!(rig.press(&[ALT, F1]), [alpha()]);
    rig.set(&[ALT], false);
    // The focus moves to the console before its notice arrives; F1 autorepeats there, now without Alt.
    rig.world.foreground = Some(OTHER);
    assert_eq!(rig.batch(&[key(F1, true, Modifiers::NONE)]), []);
}

#[test]
fn a_scrolling_key_held_through_a_barrier_does_not_scroll() {
    let mut rig = Rig::new();
    rig.set(&[DOWN], true);
    rig.arbiter.close();
    rig.reopen();
    assert_eq!(rig.batch(&[key(DOWN, true, Modifiers::NONE)]), []);
    rig.release(&[DOWN]);
    assert_eq!(rig.batch(&[key(DOWN, false, Modifiers::NONE)]), []);
    rig.set(&[DOWN], true);
    let down = Command::Press(Press::plain(ConsoleKey::Down));
    assert_eq!(rig.batch(&[key(DOWN, true, Modifiers::NONE)]), [down]);
    assert_eq!(
        rig.batch(&[key(DOWN, true, Modifiers::NONE)]),
        [down],
        "it scrolls on"
    );
}

#[test]
fn console_commands_keep_record_order() {
    let mut rig = Rig::new();
    rig.classic_front();
    let records = [
        alt_down(F1),
        alt_up(F1),
        key(ALT, false, Modifiers::NONE),
        key(0x52, true, Modifiers::NONE),
    ];
    let r = Command::Press(Press::plain(ConsoleKey::Key(Key::letter(b'R').unwrap())));
    assert_eq!(rig.batch(&records), [alpha(), r]);
}

#[test]
fn a_capture_turns_bound_chords_into_presses() {
    let mut rig = Rig::new();
    let epoch = rig.arbiter.rebind(&Bindings::defaults(&profile()), true);
    let flush = rig.arbiter.barrier(epoch.unwrap()).unwrap();
    rig.arbiter.open(flush, &rig.world);
    rig.set(&[ALT, F1], true);
    let chord = Press {
        key: ConsoleKey::Key(Key::function(1).unwrap()),
        modifiers: Modifiers::ALT,
    };
    assert_eq!(rig.batch(&[alt_down(F1)]), [Command::Press(chord)]);
    rig.game_front();
    rig.release(&[F1]);
    assert_eq!(rig.press(&[F1]), [], "no hotkey acts during a capture");
}

#[test]
fn a_key_held_through_a_rebind_cannot_act_on_its_new_binding() {
    let mut rig = Rig::new();
    let p = profile();
    let mut bindings = Bindings::defaults(&p);
    rig.game_front();
    rig.set(&[ALT, F8], true);
    assert_eq!(rig.wake(), [Command::Hotkey(Target::Master)]);
    // While F8 is held, alpha moves to Alt+F8 and the master to nothing.
    let alpha_id = p.toggle_id("alpha").unwrap();
    bindings
        .assign(Target::Master, crate::chord::Binding(None))
        .unwrap();
    let chord = Chord::new(Modifiers::ALT, Key::function(8).unwrap());
    bindings
        .assign(Target::Toggle(alpha_id), crate::chord::Binding(Some(chord)))
        .unwrap();
    let epoch = rig.arbiter.rebind(&bindings, false).unwrap();
    assert!(!rig.arbiter.is_open());
    let flush = rig.arbiter.barrier(epoch).unwrap();
    rig.arbiter.open(flush, &rig.world);
    assert_eq!(rig.wake(), []);
    rig.release(&[F8]);
    assert_eq!(rig.press(&[F8]), [alpha()]);
}

// ----- the gate -----

#[test]
fn nothing_acts_while_the_gate_is_closed_or_on_keys_held_through_it() {
    let mut rig = Rig::new();
    rig.game_front();
    rig.arbiter.close();
    assert_eq!(rig.press(&[ALT, F1]), []);
    assert_eq!(rig.batch(&[alt_down(F2)]), []);
    rig.set(&[F2], true);
    rig.reopen();
    assert_eq!(rig.wake(), []);
    assert_eq!(rig.batch(&[alt_down(F1), alt_down(F2)]), []);
    rig.release(&[F1]);
    assert_eq!(rig.press(&[F1]), [alpha()]);
}

#[test]
fn a_held_dashboard_key_does_not_repeat_through_a_barrier() {
    let mut rig = Rig::new();
    rig.set(&[0x20], true);
    let space = Command::Press(Press::plain(ConsoleKey::Key(Key::SPACE)));
    assert_eq!(rig.batch(&[key(0x20, true, Modifiers::NONE)]), [space]);
    rig.arbiter.close();
    rig.reopen();
    assert_eq!(rig.batch(&[key(0x20, true, Modifiers::NONE)]), []);
}

#[test]
fn only_the_awaited_barrier_opens_the_gate() {
    let mut rig = Rig::new();
    let first = rig.arbiter.reopen().unwrap();
    let second = rig.arbiter.reopen().unwrap();
    assert!(first < second);
    assert_eq!(rig.arbiter.barrier(first), None);
    let flush = rig.arbiter.barrier(second).unwrap();
    // A close between the acknowledgement and the flush wins.
    rig.arbiter.close();
    rig.arbiter.open(flush, &rig.world);
    assert!(!rig.arbiter.is_open());
}

#[test]
fn a_shut_gate_never_reopens() {
    let mut rig = Rig::new();
    let epoch = rig.arbiter.epoch();
    rig.arbiter.shut();
    assert!(rig.arbiter.epoch() > epoch);
    assert_eq!(rig.arbiter.reopen(), None);
    assert_eq!(
        rig.arbiter.rebind(&Bindings::defaults(&profile()), false),
        None
    );
    rig.arbiter.close();
    rig.game_front();
    assert_eq!(rig.press(&[ALT, F1]), []);
}

#[test]
fn a_failed_reader_authorises_nothing_but_the_game_path_keeps_working() {
    let mut rig = Rig::new();
    rig.press(&[ALT, F2]);
    rig.arbiter.console_failed();
    assert!(!rig.arbiter.reads_console());
    assert_eq!(rig.batch(&[alt_down(F2)]), []);
    assert_eq!(rig.batch(&[key(DOWN, true, Modifiers::NONE)]), []);
    rig.game_front();
    assert_eq!(rig.press(&[F1]), [alpha()]);
}

// ----- generated traces -----

/// The keys the traces use: two bound keys, two modifiers, and one key no binding uses. Few keys make deep
/// interleavings likely.
const KEYS: [u8; 5] = [F1, F2, ALT, SHIFT, KEY_A];

#[derive(Clone, Debug)]
enum Op {
    /// A key changes physically, with no wake.
    Set(usize, bool),
    /// A key changes and Raw Input wakes the trainer.
    Wake(usize, bool),
    /// A key changes and the console records it, in a batch with these extra records.
    Typed(Vec<(usize, bool)>, bool, bool),
    /// The console reads an autorepeat record of a key it saw go down, queued before anything since.
    Stale(usize),
    Foreground(u8),
    /// The foreground changes and the notice has not arrived yet.
    Drift(u8),
    Devices,
    Close,
    Reopen,
    Capture(bool),
}

fn op() -> impl Strategy<Value = Op> {
    let index = 0..KEYS.len();
    // Mostly key traffic, with an occasional gate change, so traces reach deep states with the gate open.
    prop_oneof![
        3 => (index.clone(), any::<bool>()).prop_map(|(k, d)| Op::Set(k, d)),
        5 => (index.clone(), any::<bool>()).prop_map(|(k, d)| Op::Wake(k, d)),
        5 => (
            prop::collection::vec((index, any::<bool>()), 0..5),
            prop::bool::weighted(0.1),
            prop::bool::weighted(0.9)
        )
            .prop_map(|(keys, focus, complete)| Op::Typed(keys, focus, complete)),
        3 => (0..KEYS.len()).prop_map(Op::Stale),
        2 => (0u8..6).prop_map(Op::Foreground),
        2 => (0u8..6).prop_map(Op::Drift),
        1 => Just(Op::Devices),
        1 => Just(Op::Close),
        2 => Just(Op::Reopen),
        1 => prop::bool::weighted(0.3).prop_map(Op::Capture),
    ]
}

/// What a generated trace must never break.
struct Oracle {
    /// Presses of the key since it was last primed that have not acted yet.
    armed: [u32; 256],
    /// The console read the key's down record and not yet its up record.
    console_down: [bool; 256],
}

impl Oracle {
    fn prime(&mut self, world: &World) {
        for vk in KEYS {
            if world.down[usize::from(vk)] {
                self.armed[usize::from(vk)] = 0;
            }
        }
    }

    fn change(&mut self, world: &mut World, vk: u8, down: bool) {
        if down && !world.down[usize::from(vk)] {
            self.armed[usize::from(vk)] += 1;
        }
        world.down[usize::from(vk)] = down;
    }

    fn check(&mut self, commands: &[Command], open: bool, capturing: bool) {
        let bindings = Bindings::defaults(&profile());
        for command in commands {
            let vk = match command {
                Command::Hotkey(target) => {
                    assert!(open && !capturing, "a hotkey acted through a closed gate");
                    bindings.get(*target).0.unwrap().key().vk()
                }
                Command::Press(Press {
                    key: ConsoleKey::Key(key),
                    ..
                }) => key.vk(),
                _ => continue,
            };
            let armed = &mut self.armed[usize::from(vk)];
            assert!(
                *armed > 0,
                "{vk:#04x} acted twice for one press, or while primed"
            );
            *armed -= 1;
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn one_physical_press_acts_at_most_once(ops in prop::collection::vec(op(), 1..60)) {
        run(ops);
    }
}

#[test]
fn the_trace_oracle_sees_a_game_press_replayed_by_the_console() {
    // Alt+F1 acts in the game; the focus moves to a classic window behind with no notice yet; the console
    // reports F1. Run against an arbiter that ignored spent holds, this trace fails.
    run(vec![
        Op::Foreground(0),
        Op::Wake(2, true),
        Op::Wake(0, true),
        Op::Drift(1),
        Op::Typed(vec![(0, true)], false, true),
    ]);
}

fn run(ops: Vec<Op>) {
    let mut rig = Rig::new();
    let mut oracle = Oracle {
        armed: [0; 256],
        console_down: [false; 256],
    };
    let mut capturing = false;
    for op in ops {
        let open = rig.arbiter.is_open();
        let commands = match op.clone() {
            Op::Set(k, down) => {
                oracle.change(&mut rig.world, KEYS[k], down);
                Vec::new()
            }
            Op::Wake(k, down) => {
                oracle.change(&mut rig.world, KEYS[k], down);
                rig.wake()
            }
            Op::Typed(keys, focus, complete) => {
                let mut records = Vec::new();
                for (k, down) in keys {
                    oracle.change(&mut rig.world, KEYS[k], down);
                    oracle.console_down[usize::from(KEYS[k])] = down;
                    let modifiers = rig.world.modifiers().unwrap_or_default();
                    records.push(key(KEYS[k], down, modifiers));
                }
                if focus {
                    records.push(Record::Focus);
                    oracle.console_down = [false; 256];
                }
                let commands = rig.read(&records, complete);
                if focus || !complete {
                    oracle.check(&commands, open, capturing);
                    oracle.prime(&rig.world);
                    continue;
                }
                commands
            }
            Op::Stale(k) => {
                let vk = KEYS[k];
                if !oracle.console_down[usize::from(vk)] {
                    continue;
                }
                let modifiers = rig.world.modifiers().unwrap_or_default();
                rig.batch(&[key(vk, true, modifiers)])
            }
            Op::Foreground(which) | Op::Drift(which) => {
                rig.world.foreground = Some(if which % 2 == 0 { GAME } else { OTHER });
                rig.world.console = [
                    ConsoleFocus::Front,
                    ConsoleFocus::Behind,
                    ConsoleFocus::Hidden,
                ][usize::from(which % 3)];
                if matches!(op, Op::Foreground(_)) {
                    rig.arbiter.foreground(&rig.world);
                    oracle.prime(&rig.world);
                }
                Vec::new()
            }
            Op::Devices => {
                rig.arbiter.devices(&rig.world);
                oracle.prime(&rig.world);
                Vec::new()
            }
            Op::Close => {
                rig.arbiter.close();
                Vec::new()
            }
            Op::Reopen => {
                rig.reopen();
                oracle.prime(&rig.world);
                oracle.console_down = [false; 256];
                Vec::new()
            }
            Op::Capture(on) => {
                capturing = on;
                if let Some(epoch) = rig.arbiter.rebind(&Bindings::defaults(&profile()), on) {
                    let flush = rig.arbiter.barrier(epoch).unwrap();
                    rig.arbiter.open(flush, &rig.world);
                    oracle.prime(&rig.world);
                    oracle.console_down = [false; 256];
                }
                Vec::new()
            }
        };
        oracle.check(&commands, open, capturing);
    }
}
