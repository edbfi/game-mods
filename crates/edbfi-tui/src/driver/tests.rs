// SPDX-License-Identifier: AGPL-3.0-only
//! The driver against a pretend runtime and platform: wakes, the input gate, Ctrl+C, saves and a lost
//! console.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use edbfi_trainer::chord::Modifiers;
use edbfi_trainer::control::Ack;
use edbfi_trainer::input::{ConsoleFocus, KeyRecord, Record};
use edbfi_trainer::lifecycle::Finish;
use edbfi_trainer::profile::ValidProfile;
use edbfi_trainer::profile::synthetic::SYNTHETIC;

use super::*;
use crate::app::{Modal, Paths, Startup};
use crate::theme::Appearance;

const GAME: u32 = 4242;
const VK_F1: u8 = 0x70;
const VK_R: u8 = 0x52;

/// What the pretend runtime was asked to do.
#[derive(Debug, Default)]
struct Calls {
    commands: Vec<RuntimeCommand>,
    saves: usize,
    stops: usize,
    shutdowns: usize,
    exits: usize,
    steps: usize,
}

/// A runtime whose phase the test sets.
#[derive(Debug, Default)]
struct Pretend {
    calls: Rc<RefCell<Calls>>,
    events: Vec<RuntimeEvent>,
    attached: bool,
    finished: bool,
    revision: u64,
}

impl Pretend {
    fn enter(&mut self, phase: Phase) {
        self.attached = phase.is_attached();
        self.finished = matches!(phase, Phase::Finished(_));
        self.events.push(RuntimeEvent::Phase(phase));
    }
}

impl Backend for Pretend {
    fn command(&mut self, command: RuntimeCommand) {
        self.calls.borrow_mut().commands.push(command);
        match command {
            RuntimeCommand::Publish(mask) if self.attached => {
                self.revision += 1;
                self.events
                    .push(RuntimeEvent::Ack(Ack::simulated(self.revision, mask)));
            }
            RuntimeCommand::Quit if self.attached => self.enter(Phase::Restoring),
            RuntimeCommand::Quit => self.enter(Phase::Finished(Finish::Untouched)),
            RuntimeCommand::Retry => self.enter(Phase::Waiting),
            _ => {}
        }
    }

    fn save(&mut self, _settings: &Settings) -> Result<Saved, StoreError> {
        self.calls.borrow_mut().saves += 1;
        Ok(Saved::Written { external: None })
    }

    fn step(&mut self) {
        self.calls.borrow_mut().steps += 1;
    }

    fn stop(&mut self) {
        self.calls.borrow_mut().stops += 1;
        self.enter(Phase::Restoring);
    }

    fn shutdown(&mut self) {
        self.calls.borrow_mut().shutdowns += 1;
        self.enter(Phase::Restoring);
    }

    fn exited(&mut self) {
        self.calls.borrow_mut().exits += 1;
    }

    fn events(&mut self) -> Vec<RuntimeEvent> {
        std::mem::take(&mut self.events)
    }

    fn game(&self) -> Option<u32> {
        self.attached.then_some(GAME)
    }

    fn finished(&self) -> bool {
        self.finished
    }
}

/// The keyboard and windows as the test sets them.
#[derive(Debug, Default)]
struct Keys {
    down: RefCell<Vec<u8>>,
    modifiers: RefCell<Modifiers>,
    foreground: RefCell<Option<u32>>,
}

impl Probe for Keys {
    fn down(&self, vk: u8) -> bool {
        self.down.borrow().contains(&vk)
    }

    fn modifiers(&self) -> Option<Modifiers> {
        Some(*self.modifiers.borrow())
    }

    fn foreground(&self) -> Option<u32> {
        *self.foreground.borrow()
    }

    fn console(&self) -> ConsoleFocus {
        ConsoleFocus::Front
    }
}

/// What the pretend platform was asked to do.
#[derive(Debug, Default)]
struct Screen {
    barriers: Vec<Epoch>,
    batches: VecDeque<Result<Batch, OsError>>,
    flushes: usize,
    titles: Vec<String>,
    chimes: Vec<Chime>,
    frames: usize,
    fail_draw: bool,
    left: bool,
    printed: Vec<String>,
}

#[derive(Debug, Default)]
struct Platform {
    keys: Keys,
    screen: Rc<RefCell<Screen>>,
    /// Whether barriers are acknowledged through keyboard wakes, as on Windows.
    notices: bool,
}

impl Host for Platform {
    type Probe = Keys;

    fn probe(&self) -> &Keys {
        &self.keys
    }

    fn barrier(&mut self, epoch: Epoch) -> bool {
        self.screen.borrow_mut().barriers.push(epoch);
        self.notices
    }

    fn read_console(&mut self) -> Result<Batch, OsError> {
        self.screen
            .borrow_mut()
            .batches
            .pop_front()
            .unwrap_or_else(|| {
                Ok(Batch {
                    complete: true,
                    ..Batch::default()
                })
            })
    }

    fn flush_console(&mut self) -> Result<(), OsError> {
        self.screen.borrow_mut().flushes += 1;
        Ok(())
    }

    fn size(&mut self) -> (u16, u16) {
        (72, 24)
    }

    fn chime(&mut self, chime: Chime) {
        self.screen.borrow_mut().chimes.push(chime);
    }

    fn title(&mut self, title: &str) {
        self.screen.borrow_mut().titles.push(title.to_owned());
    }

    fn draw(&mut self, _app: &App<'_>) -> std::io::Result<()> {
        let mut screen = self.screen.borrow_mut();
        if screen.fail_draw {
            return Err(std::io::Error::other("the console went away"));
        }
        screen.frames += 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.screen.borrow_mut().left = true;
    }

    fn print(&mut self, lines: &[String]) {
        self.screen.borrow_mut().printed.extend_from_slice(lines);
    }
}

struct Rig {
    driver: Driver<'static, Pretend, Platform>,
    calls: Rc<RefCell<Calls>>,
    screen: Rc<RefCell<Screen>>,
}

fn rig_with(phase: Phase, notices: bool, console: bool) -> Rig {
    let profile = ValidProfile::new(&SYNTHETIC).unwrap();
    let mut settings = Settings::defaults(&profile);
    settings.sounds = true;
    let app = App::new(
        profile,
        Startup {
            settings,
            paths: Paths {
                settings: Some(PathBuf::from("settings.toml")),
                log: None,
            },
            phase,
            appearance: Appearance::default(),
            size: (100, 30),
        },
    );
    let backend = Pretend::default();
    let calls = Rc::clone(&backend.calls);
    let host = Platform {
        notices,
        ..Platform::default()
    };
    let screen = Rc::clone(&host.screen);
    Rig {
        driver: Driver::new(app, backend, host, console),
        calls,
        screen,
    }
}

fn fresh() -> Rig {
    rig_with(Phase::Waiting, true, true)
}

impl Rig {
    /// Acknowledges the newest barrier, as the platform would.
    fn acknowledge(&mut self) {
        let epoch = *self.screen.borrow().barriers.last().unwrap();
        self.driver.wake(Wake::Keyboard(Wakes {
            barrier: Some(epoch),
            ..Wakes::default()
        }));
    }

    fn runtime(&mut self, phase: Phase) {
        self.driver.backend_mut().enter(phase);
        self.driver.wake(Wake::Due);
    }

    fn attach(&mut self) {
        self.runtime(Phase::Attached {
            attach_duration: Duration::from_millis(5),
        });
    }

    fn publications(&self) -> usize {
        self.calls
            .borrow()
            .commands
            .iter()
            .filter(|command| matches!(command, RuntimeCommand::Publish(_)))
            .count()
    }

    /// Presses Alt+F1 with the game in front: the game path.
    fn hotkey(&mut self) {
        let keys = &self.driver.host().keys;
        *keys.modifiers.borrow_mut() = Modifiers::ALT;
        *keys.foreground.borrow_mut() = Some(GAME);
        keys.down.borrow_mut().push(VK_F1);
        self.driver.wake(Wake::Keyboard(Wakes {
            keyboard: true,
            ..Wakes::default()
        }));
        self.driver.host().keys.down.borrow_mut().clear();
        self.driver.wake(Wake::Keyboard(Wakes {
            keyboard: true,
            ..Wakes::default()
        }));
    }

    fn console(&mut self, records: Vec<Record>) {
        self.screen.borrow_mut().batches.push_back(Ok(Batch {
            records,
            complete: true,
            resized: false,
        }));
        self.driver.wake(Wake::Console);
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

#[test]
fn the_start_sets_the_title_draws_once_and_opens_the_gate_through_a_barrier() {
    let mut rig = fresh();
    assert_eq!(rig.screen.borrow().titles.len(), 1);
    assert_eq!(rig.screen.borrow().frames, 1);
    assert!(!rig.driver.arbiter().is_open());
    assert_eq!(rig.screen.borrow().barriers.len(), 1);
    rig.acknowledge();
    assert!(rig.driver.arbiter().is_open());
    assert_eq!(
        rig.screen.borrow().flushes,
        1,
        "the console is flushed first"
    );
    // A wake that changes nothing visible draws nothing.
    rig.driver.wake(Wake::Due);
    assert_eq!(rig.screen.borrow().frames, 1);
    assert_eq!(rig.calls.borrow().steps, 1);
}

#[test]
fn without_keyboard_notices_the_driver_acknowledges_its_own_barriers() {
    let rig = rig_with(Phase::Waiting, false, true);
    assert!(rig.driver.arbiter().is_open());
}

#[test]
fn a_game_hotkey_publishes_and_chimes_once_attached() {
    let mut rig = fresh();
    rig.acknowledge();
    rig.attach();
    assert_eq!(rig.publications(), 1, "attaching publishes the selection");
    let chimes = rig.screen.borrow().chimes.len();
    rig.hotkey();
    assert_eq!(rig.publications(), 2);
    assert_eq!(rig.screen.borrow().chimes.len(), chimes + 1);
    assert_eq!(rig.calls.borrow().saves, 1, "the toggle is saved");
}

#[test]
fn writing_code_closes_the_gate_and_keys_held_across_it_never_act() {
    let mut rig = fresh();
    rig.acknowledge();
    rig.runtime(Phase::Installing);
    assert!(!rig.driver.arbiter().is_open());
    let barriers = rig.screen.borrow().barriers.len();
    rig.attach();
    assert_eq!(rig.screen.borrow().barriers.len(), barriers + 1);
    assert!(
        !rig.driver.arbiter().is_open(),
        "open only once acknowledged"
    );
    // Alt+F1 pressed before the acknowledgement is primed, not acted on.
    let keys = &rig.driver.host().keys;
    *keys.modifiers.borrow_mut() = Modifiers::ALT;
    *keys.foreground.borrow_mut() = Some(GAME);
    keys.down.borrow_mut().push(VK_F1);
    rig.driver.wake(Wake::Keyboard(Wakes {
        keyboard: true,
        ..Wakes::default()
    }));
    rig.acknowledge();
    rig.driver.wake(Wake::Keyboard(Wakes {
        keyboard: true,
        ..Wakes::default()
    }));
    assert_eq!(rig.publications(), 1, "only the attach publication");
    rig.driver.host().keys.down.borrow_mut().clear();
    rig.driver.wake(Wake::Keyboard(Wakes {
        keyboard: true,
        ..Wakes::default()
    }));
    rig.hotkey();
    assert_eq!(rig.publications(), 2);
}

#[test]
fn a_retry_closes_the_gate_before_the_runtime_runs_it() {
    let mut rig = fresh();
    rig.acknowledge();
    rig.runtime(Phase::Failed(edbfi_trainer::lifecycle::Failure {
        recovery: edbfi_trainer::recovery::RecoveryAction::Retry,
        message: "no".into(),
        unrestored: Vec::new(),
    }));
    let barriers = rig.screen.borrow().barriers.len();
    rig.console(vec![
        key(0x0D, true, Modifiers::NONE),
        key(0x0D, false, Modifiers::NONE),
    ]);
    assert!(rig.calls.borrow().commands.contains(&RuntimeCommand::Retry));
    assert_eq!(rig.screen.borrow().barriers.len(), barriers + 1);
    assert!(!rig.driver.arbiter().is_open());
}

#[test]
fn ctrl_c_asks_first_and_a_second_press_quits() {
    let mut rig = fresh();
    rig.acknowledge();
    rig.attach();
    rig.driver.wake(Wake::Interrupts(1));
    assert!(matches!(
        rig.driver.app().model.modal,
        Some(Modal::Quit { .. })
    ));
    assert!(!rig.calls.borrow().commands.contains(&RuntimeCommand::Quit));
    rig.driver.wake(Wake::Interrupts(1));
    assert!(rig.calls.borrow().commands.contains(&RuntimeCommand::Quit));
    assert!(!rig.driver.arbiter().is_open(), "restoring closes the gate");

    // Two presses before the owner woke count as both.
    let mut rig = fresh();
    rig.attach();
    rig.driver.wake(Wake::Interrupts(5));
    let quits = rig
        .calls
        .borrow()
        .commands
        .iter()
        .filter(|command| **command == RuntimeCommand::Quit)
        .count();
    assert_eq!(quits, 1);
}

#[test]
fn a_stop_or_shutdown_restores_without_asking() {
    let mut rig = fresh();
    rig.attach();
    rig.driver.wake(Wake::Stop);
    assert_eq!(rig.calls.borrow().stops, 1);
    assert!(rig.driver.app().model.modal.is_none());
    assert_eq!(*rig.driver.app().phase(), Phase::Restoring);
    rig.runtime(Phase::Finished(Finish::Restored));
    assert!(rig.driver.finished());
    // A shut gate never reopens.
    assert!(!rig.driver.arbiter().is_open());
    let barriers = rig.screen.borrow().barriers.len();
    rig.driver.wake(Wake::Due);
    assert_eq!(rig.screen.borrow().barriers.len(), barriers);

    let mut rig = fresh();
    rig.driver.wake(Wake::Shutdown);
    assert_eq!(rig.calls.borrow().shutdowns, 1);
    rig.driver.wake(Wake::Exited);
    assert_eq!(rig.calls.borrow().exits, 1);
}

#[test]
fn capturing_a_binding_is_a_barrier_and_console_keys_reach_the_dialog() {
    let mut rig = fresh();
    rig.acknowledge();
    let barriers = rig.screen.borrow().barriers.len();
    rig.console(vec![
        key(VK_R, true, Modifiers::NONE),
        key(VK_R, false, Modifiers::NONE),
    ]);
    assert!(rig.driver.app().capturing());
    assert_eq!(rig.screen.borrow().barriers.len(), barriers + 1);
    rig.acknowledge();
    // While capturing, a bound chord is a key for the dialog, not a hotkey.
    rig.console(vec![
        key(VK_F1, true, Modifiers::ALT),
        key(VK_F1, false, Modifiers::ALT),
    ]);
    let Some(Modal::Capture { candidate, .. }) = &rig.driver.app().model.modal else {
        panic!("{:?}", rig.driver.app().model.modal);
    };
    assert!(candidate.is_some());
    assert_eq!(rig.publications(), 0);
}

#[test]
fn keys_after_a_capture_starts_in_the_same_batch_never_reach_the_dialog() {
    let mut rig = fresh();
    rig.acknowledge();
    // R opens the capture; the chord and Enter typed before its barrier must not set a binding.
    rig.console(vec![
        key(VK_R, true, Modifiers::NONE),
        key(VK_R, false, Modifiers::NONE),
        key(0x77, true, Modifiers::ALT),
        key(0x77, false, Modifiers::ALT),
        key(0x0D, true, Modifiers::NONE),
        key(0x0D, false, Modifiers::NONE),
    ]);
    let Some(Modal::Capture { candidate, .. }) = &rig.driver.app().model.modal else {
        panic!("{:?}", rig.driver.app().model.modal);
    };
    assert!(candidate.is_none());
    assert_eq!(rig.calls.borrow().saves, 0);
}

#[test]
fn a_key_held_when_the_game_comes_to_the_front_does_not_act() {
    let mut rig = fresh();
    rig.acknowledge();
    rig.attach();
    // Alt+F1 went down in another window; the focus moved to the game before the owner woke.
    let keys = &rig.driver.host().keys;
    *keys.modifiers.borrow_mut() = Modifiers::ALT;
    *keys.foreground.borrow_mut() = Some(GAME);
    keys.down.borrow_mut().push(VK_F1);
    rig.driver.wake(Wake::Keyboard(Wakes {
        keyboard: true,
        foreground: true,
        ..Wakes::default()
    }));
    assert_eq!(rig.publications(), 1, "only the attach publication");
}

#[test]
fn a_resized_console_resizes_the_dashboard() {
    let mut rig = fresh();
    rig.screen.borrow_mut().batches.push_back(Ok(Batch {
        records: Vec::new(),
        complete: true,
        resized: true,
    }));
    let frames = rig.screen.borrow().frames;
    rig.driver.wake(Wake::Console);
    assert_eq!(rig.driver.app().model.size, (72, 24));
    assert_eq!(rig.screen.borrow().frames, frames + 1);
}

#[test]
fn a_lost_console_switches_to_plain_lines_and_is_no_longer_read() {
    let mut rig = fresh();
    rig.acknowledge();
    rig.attach();
    rig.driver.wake(Wake::Interrupts(1));
    rig.screen
        .borrow_mut()
        .batches
        .push_back(Err(OsError::new("ReadConsoleInputW", 6)));
    rig.driver.wake(Wake::Console);
    assert!(!rig.driver.reads_console());
    assert!(rig.driver.app().plain());
    assert!(
        rig.driver.app().model.modal.is_none(),
        "no dialog can trap the trainer"
    );
    assert!(rig.screen.borrow().left);
    assert!(
        rig.screen.borrow().printed[0].starts_with("edbfi"),
        "the whole state is printed"
    );
    // The game path keeps working, and a plain Ctrl+C quits at once.
    rig.hotkey();
    assert_eq!(rig.publications(), 2);
    let printed = rig.screen.borrow().printed.len();
    rig.driver.wake(Wake::Interrupts(1));
    assert!(rig.calls.borrow().commands.contains(&RuntimeCommand::Quit));
    assert!(
        rig.screen.borrow().printed.len() > printed,
        "changes are printed"
    );
}

#[test]
fn a_failed_draw_switches_to_plain_lines() {
    let mut rig = fresh();
    rig.screen.borrow_mut().fail_draw = true;
    rig.attach();
    assert!(rig.driver.app().plain());
    assert!(rig.screen.borrow().left);
    assert!(!rig.screen.borrow().printed.is_empty());
}

#[test]
fn a_run_without_a_console_prints_from_the_start() {
    let rig = rig_with(Phase::Waiting, true, false);
    assert!(rig.driver.app().plain());
    assert_eq!(rig.screen.borrow().frames, 0);
    assert!(rig.screen.borrow().printed[0].starts_with("edbfi"));
}
