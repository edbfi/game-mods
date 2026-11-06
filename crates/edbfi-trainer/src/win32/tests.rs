// SPDX-License-Identifier: AGPL-3.0-only
//! The whole lifecycle against a real process: the `dummy_game` example, attached, toggled, paused,
//! restored and attached again, plus the close, session-end and panic paths. Run by `cargo test`, which
//! builds the example first.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use windows_sys::Win32::System::Console::{
    CTRL_CLOSE_EVENT, INPUT_RECORD, KEY_EVENT, KEY_EVENT_RECORD, KEY_EVENT_RECORD_0,
    WriteConsoleInputW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SendMessageW, WM_ENDSESSION, WM_QUERYENDSESSION,
};

use crate::addr::Va;
use crate::control::{ControlBit, ControlSet, DesiredMask};
use crate::patch::{Process as _, RegionState};
use crate::profile::ValidProfile;
use crate::profile::synthetic::SYNTHETIC;
use crate::session::{
    Candidate, Installed, ReadOnly, SelectError, Session, StopOutcome, TextCheck, TrainerError,
    VerifyRestore, Write, identify, request_stop, select,
};

use super::bridge;
use super::process::WinGame;

/// The bridge, its shutdown event and its lease slot are process-wide, so these tests take turns.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Takes this test's turn, with no shutdown request or interrupt routing left by an earlier test.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    let turn = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    bridge::reset_for_tests();
    turn
}

fn dummy_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let path = exe
        .parent()
        .and_then(|deps| deps.parent())
        .unwrap()
        .join("examples")
        .join("dummy_game.exe");
    assert!(
        path.exists(),
        "{} is missing: run `cargo test` without target filters so the example is built",
        path.display()
    );
    path
}

/// The dummy game, running.
struct Dummy {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    /// Its first line of output.
    layout: String,
}

impl Dummy {
    fn start() -> Self {
        let mut child = Command::new(dummy_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut layout = String::new();
        output.read_line(&mut layout).unwrap();
        Self {
            child,
            input,
            output,
            layout,
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn ask(&mut self, command: &str) -> String {
        writeln!(self.input, "{command}").unwrap();
        self.input.flush().unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        line.trim().to_owned()
    }
}

impl Drop for Dummy {
    fn drop(&mut self) {
        let _killed = self.child.kill();
        let _waited = self.child.wait();
    }
}

/// The dummy's profile.
fn profile(dummy: &Dummy) -> ValidProfile<'static> {
    let file = std::fs::read(dummy_path()).unwrap();
    ValidProfile::new(crate::profile::dummy::profile(&file, &dummy.layout).unwrap()).unwrap()
}

fn mask(bits: &[u8]) -> DesiredMask {
    DesiredMask::new(bits.iter().fold(ControlSet::EMPTY, |set, &bit| {
        set.with(ControlBit::new(bit))
    }))
}

fn attach(dummy: &Dummy, profile: &ValidProfile<'static>) -> Session<Installed> {
    let candidate = Candidate::<Write>::open(dummy.pid()).unwrap();
    identify(candidate, profile)
        .unwrap()
        .claim()
        .unwrap()
        .prepare()
        .unwrap()
        .install()
        .unwrap()
}

/// The game's code at every site, read back.
fn sites(dummy: &Dummy, profile: &ValidProfile<'static>, image: Va) -> Vec<Vec<u8>> {
    let game = WinGame::open(dummy.pid(), false).unwrap();
    profile
        .profile()
        .sites
        .iter()
        .map(|site| {
            let mut bytes = vec![0; site.original.len()];
            game.read(image.at(site.rva).unwrap(), &mut bytes).unwrap();
            bytes
        })
        .collect()
}

fn originals(profile: &ValidProfile<'static>) -> Vec<Vec<u8>> {
    profile
        .profile()
        .sites
        .iter()
        .map(|site| site.original.to_vec())
        .collect()
}

#[test]
fn a_real_process_is_installed_toggled_paused_and_restored() {
    let _serial = serial();
    let mut dummy = Dummy::start();
    let profile = profile(&dummy);
    assert_eq!(dummy.ask("alpha"), "alpha 0");
    assert_eq!(dummy.ask("drain"), "battery 99");
    assert_eq!(dummy.ask("fire"), "rounds 29");

    // A read-only check identifies the game and cannot go further.
    let check = Candidate::<ReadOnly>::open(dummy.pid()).unwrap();
    let checked = identify(check, &profile).unwrap();
    let image = checked.image();
    drop(checked);

    // A second trainer that identified the game before this one hooks it is refused when it claims; once
    // hooked, the game no longer even identifies.
    let other = identify(Candidate::<Write>::open(dummy.pid()).unwrap(), &profile).unwrap();
    let mut session = attach(&dummy, &profile);
    assert_eq!(session.image(), image);
    let hooked = identify(Candidate::<ReadOnly>::open(dummy.pid()).unwrap(), &profile);
    assert!(hooked.unwrap_err().error.transient());
    assert!(
        sites(&dummy, &profile, image)
            .iter()
            .all(|site| site[0] == 0xE9)
    );
    // Installed with every cheat off: the game behaves as before.
    assert_eq!(dummy.ask("alpha"), "alpha 0");
    assert_eq!(dummy.ask("drain"), "battery 98");
    assert_eq!(dummy.ask("fire"), "rounds 28");

    session.publish(mask(&[0, 1, 2])).unwrap();
    assert_eq!(dummy.ask("alpha"), "alpha 1");
    assert_eq!(dummy.ask("drain"), "battery 98");
    assert_eq!(dummy.ask("fire"), "rounds 28");
    // Only the red class is kept; a blue selection leaves the red weapon spending rounds.
    session.publish(mask(&[3])).unwrap();
    assert_eq!(dummy.ask("fire"), "rounds 27");
    // Paused.
    session.publish(DesiredMask::ZERO).unwrap();
    assert_eq!(dummy.ask("alpha"), "alpha 0");
    assert_eq!(dummy.ask("drain"), "battery 97");

    // Another trainer is refused while this one owns the game.
    assert!(matches!(
        other.claim().unwrap_err().error,
        TrainerError::Claim(_)
    ));

    // A stop request reaches the owner without touching the game.
    let outcomes = request_stop(profile.profile(), Some(dummy.pid())).unwrap();
    assert_eq!(outcomes, [(dummy.pid(), StopOutcome::Requested)]);
    assert!(session.stop_requested());

    let detached = session.restore(VerifyRestore::Text).unwrap();
    assert_eq!(detached.report().text, TextCheck::Identical);
    assert_eq!(sites(&dummy, &profile, image), originals(&profile));
    assert_eq!(dummy.ask("alpha"), "alpha 0");
    drop(detached);
    assert_eq!(
        request_stop(profile.profile(), Some(dummy.pid())).unwrap(),
        [(dummy.pid(), StopOutcome::NoTrainer)]
    );
}

#[test]
fn a_setter_blocked_across_detach_returns_into_the_retained_cave() {
    let _serial = serial();
    let mut dummy = Dummy::start();
    let profile = profile(&dummy);
    let mut session = attach(&dummy, &profile);
    session.publish(mask(&[2])).unwrap();
    assert_eq!(dummy.ask("fire-held"), "held");
    let image = session.image();
    let detached = session.restore(VerifyRestore::Text).unwrap();
    assert_eq!(sites(&dummy, &profile, image), originals(&profile));
    drop(detached);
    // The setter returns into the cave it was called from, which must still be there.
    assert_eq!(dummy.ask("release"), "rounds 30");
    assert_eq!(dummy.ask("fire"), "rounds 29");
    // A second attach to the same game gets fresh caves and controls.
    let mut session = attach(&dummy, &profile);
    session.publish(mask(&[2])).unwrap();
    assert_eq!(dummy.ask("fire"), "rounds 29");
    session.restore(VerifyRestore::Sites).unwrap();
    assert_eq!(dummy.ask("fire"), "rounds 28");
}

#[test]
fn a_game_that_exits_ends_the_session_without_restoring() {
    let _serial = serial();
    let mut dummy = Dummy::start();
    let profile = profile(&dummy);
    let mut session = attach(&dummy, &profile);
    session.publish(mask(&[0])).unwrap();
    dummy.child.kill().unwrap();
    dummy.child.wait().unwrap();
    let proof = session.exit_proof().unwrap();
    session.exited(proof).unwrap();
}

/// Attaches to `dummy`, starts `notice` on another thread, then plays the owner: waits for the shutdown
/// request and restores. The notice must zero the controls at once and wait until restoration finished.
fn notice_restores(dummy: &mut Dummy, profile: &ValidProfile<'static>, notice: fn()) {
    let shutdown = bridge::install().unwrap();
    bridge::reset_for_tests();
    let mut session = attach(dummy, profile);
    session.publish(mask(&[0])).unwrap();
    assert_eq!(dummy.ask("alpha"), "alpha 1");
    let started = Instant::now();
    let waiter = std::thread::spawn(move || {
        notice();
        started.elapsed()
    });
    assert!(
        shutdown.wait(Duration::from_secs(5)),
        "the notice asks the owner to stop"
    );
    // The notice zeroed the controls before the owner did anything, and is still waiting.
    assert_eq!(dummy.ask("alpha"), "alpha 0");
    std::thread::sleep(Duration::from_millis(200));
    assert!(!waiter.is_finished(), "the notice waits for restoration");
    let image = session.image();
    session.restore(VerifyRestore::Sites).unwrap();
    let waited = waiter.join().unwrap();
    assert!(
        waited < bridge::CLOSE_BUDGET,
        "the notice returned once restored: {waited:?}"
    );
    assert_eq!(sites(dummy, profile, image), originals(profile));
}

fn close_notice() {
    assert!(bridge::console_notice(CTRL_CLOSE_EVENT));
}

fn send(message: u32, wparam: usize) -> isize {
    let window = super::window::window().unwrap();
    // SAFETY: `window` is the bridge's live hidden window; the message takes no pointers.
    unsafe { SendMessageW(window, message, wparam, 0) }
}

#[test]
fn a_closing_console_waits_for_restoration_on_every_attach() {
    let _serial = serial();
    let mut dummy = Dummy::start();
    let profile = profile(&dummy);
    // A second attach gets a fresh lease: its notice waits again rather than finding the first one done.
    notice_restores(&mut dummy, &profile, close_notice);
    notice_restores(&mut dummy, &profile, close_notice);
}

#[test]
fn a_session_end_waits_for_restoration() {
    let _serial = serial();
    let mut dummy = Dummy::start();
    let profile = profile(&dummy);
    notice_restores(&mut dummy, &profile, || {
        assert_eq!(send(WM_QUERYENDSESSION, 0), 1);
    });
    notice_restores(&mut dummy, &profile, || {
        assert_eq!(send(WM_ENDSESSION, 1), 0);
    });
}

#[test]
fn a_cancelled_session_end_changes_nothing() {
    let _serial = serial();
    let shutdown = bridge::install().unwrap();
    bridge::reset_for_tests();
    let mut dummy = Dummy::start();
    let profile = profile(&dummy);
    let mut session = attach(&dummy, &profile);
    session.publish(mask(&[0])).unwrap();
    assert_eq!(send(WM_ENDSESSION, 0), 0);
    assert!(!shutdown.requested());
    assert_eq!(dummy.ask("alpha"), "alpha 1");
    session.restore(VerifyRestore::Sites).unwrap();
}

#[test]
fn a_panic_zeroes_the_controls_at_once() {
    let _serial = serial();
    bridge::install().unwrap();
    bridge::reset_for_tests();
    let mut dummy = Dummy::start();
    let profile = profile(&dummy);
    let mut session = attach(&dummy, &profile);
    session.publish(mask(&[0])).unwrap();
    assert_eq!(dummy.ask("alpha"), "alpha 1");
    let _caught = std::thread::spawn(|| panic!("a deliberate test panic")).join();
    assert_eq!(dummy.ask("alpha"), "alpha 0");
    session.restore(VerifyRestore::Sites).unwrap();
}

#[test]
fn discovery_finds_one_game_refuses_several_and_stops_every_trainer() {
    let _serial = serial();
    let first = Dummy::start();
    let profile = profile(&first);
    let game = profile.profile();
    assert_eq!(select(game, None), Ok(first.pid()));
    assert_eq!(select(game, Some(first.pid())), Ok(first.pid()));
    assert_eq!(
        select(game, Some(std::process::id())),
        Err(SelectError::NotGame(std::process::id()))
    );
    let second = Dummy::start();
    assert_eq!(select(game, None), Err(SelectError::Ambiguous(2)));
    assert_eq!(select(game, Some(second.pid())), Ok(second.pid()));
    // One of the two has a trainer; asking every game's trainer to stop reaches only that one.
    let session = attach(&second, &profile);
    let mut outcomes = request_stop(game, None).unwrap();
    outcomes.sort_by_key(|&(pid, _)| pid != second.pid());
    assert_eq!(
        outcomes,
        [
            (second.pid(), StopOutcome::Requested),
            (first.pid(), StopOutcome::NoTrainer)
        ]
    );
    assert!(session.stop_requested());
    session.restore(VerifyRestore::Sites).unwrap();
    drop((first, second));
    assert_eq!(select(game, None), Err(SelectError::NotRunning));
}

#[test]
fn an_abandoned_preparation_takes_its_control_word_out_of_the_game() {
    let _serial = serial();
    let dummy = Dummy::start();
    let profile = profile(&dummy);
    let candidate = Candidate::<Write>::open(dummy.pid()).unwrap();
    let prepared = identify(candidate, &profile)
        .unwrap()
        .claim()
        .unwrap()
        .prepare()
        .unwrap();
    let at = prepared.controls_at();
    assert!(at.get() >= 1 << 33, "the control word is above 8 GiB: {at}");
    let game = WinGame::open(dummy.pid(), false).unwrap();
    assert_eq!(game.query(at).unwrap().state, RegionState::Committed);
    // No cave was ever published, so dropping the session frees the control word's page.
    drop(prepared);
    assert_eq!(game.query(at).unwrap().state, RegionState::Free);
}

#[test]
fn a_withdrawn_control_page_is_never_written_again() {
    use crate::session::Game as _;
    let _serial = serial();
    bridge::install().unwrap();
    bridge::reset_for_tests();
    let dummy = Dummy::start();
    let profile = profile(&dummy);
    // A boundary the trainer itself picks for the control word.
    let candidate = Candidate::<Write>::open(dummy.pid()).unwrap();
    let prepared = identify(candidate, &profile)
        .unwrap()
        .claim()
        .unwrap()
        .prepare()
        .unwrap();
    let at = prepared.controls_at();
    drop(prepared);
    let game = WinGame::open(dummy.pid(), true).unwrap();
    let mut controls = game.controls(at).unwrap();
    controls.withdraw();
    // The game takes the address for something else before the controls go.
    let reused = game.allocate(at, 4096).unwrap();
    game.write(at, &[0xAB; 4]).unwrap();
    let _caught = std::thread::spawn(|| panic!("a deliberate test panic")).join();
    drop(controls);
    let mut back = [0; 4];
    game.read(at, &mut back).unwrap();
    assert_eq!(
        back, [0xAB; 4],
        "neither the panic hook nor the drop wrote to it"
    );
    drop(reused);
}

// ----- keyboard and foreground notices, console input -----

/// Waits up to a second for the keyboard notices' event.
fn woken(keyboard: &crate::input::platform::Keyboard) -> bool {
    use std::os::windows::io::AsRawHandle as _;
    // SAFETY: the event lives for the whole process.
    let waited = unsafe {
        windows_sys::Win32::System::Threading::WaitForSingleObject(
            keyboard.event().as_raw_handle(),
            1000,
        )
    };
    waited == windows_sys::Win32::Foundation::WAIT_OBJECT_0
}

fn keyboard() -> crate::input::platform::Keyboard {
    let shutdown = crate::shutdown::install().unwrap();
    let keyboard = crate::input::platform::Keyboard::start(&shutdown).unwrap();
    let _stale = keyboard.take();
    keyboard
}

#[test]
fn keyboard_and_foreground_notices_wake_the_owner_and_merge() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{WM_INPUT, WM_INPUT_DEVICE_CHANGE};
    let _serial = serial();
    let keyboard = keyboard();
    assert!(
        !super::input::heard(WM_INPUT, 0, 0),
        "Raw Input still gets its default handling"
    );
    assert!(!super::input::heard(WM_INPUT, 0, 0));
    assert!(woken(&keyboard));
    assert!(keyboard.take().keyboard);
    assert_eq!(send(WM_INPUT_DEVICE_CHANGE, 2), 0);
    super::input::foreground_notice();
    assert!(woken(&keyboard));
    let wakes = keyboard.take();
    assert!(wakes.devices && wakes.foreground, "{wakes:?}");
}

#[test]
fn a_barrier_comes_back_through_the_window_thread() {
    let _serial = serial();
    let keyboard = keyboard();
    let profile = ValidProfile::new(&SYNTHETIC).unwrap();
    let mut arbiter = crate::input::Arbiter::new(&crate::settings::Bindings::defaults(&profile));
    let stale = arbiter.reopen().unwrap();
    let epoch = arbiter.reopen().unwrap();
    keyboard.barrier(stale).unwrap();
    keyboard.barrier(epoch).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let acknowledged = loop {
        assert!(Instant::now() < deadline, "no acknowledgement");
        if woken(&keyboard)
            && let Some(acknowledged) = keyboard.take().barrier
            && acknowledged == epoch
        {
            break acknowledged;
        }
    };
    let flush = arbiter.barrier(acknowledged).unwrap();
    arbiter.open(flush, &crate::input::platform::System);
    assert!(arbiter.is_open());
}

#[test]
fn the_system_probe_answers_without_a_desktop() {
    use crate::input::Probe as _;
    let probe = crate::input::platform::System;
    // Nobody holds F24 on a test machine.
    assert!(!probe.down(0x87));
    let _ = (probe.modifiers(), probe.foreground(), probe.console());
}

/// A console reader, allocating a console if the test process has none.
fn reader() -> super::console::Reader {
    super::console::Reader::open().unwrap_or_else(|_| {
        // SAFETY: the test process has no console, so this gives it one.
        let allocated = unsafe { windows_sys::Win32::System::Console::AllocConsole() };
        assert_ne!(allocated, 0, "no console and none can be allocated");
        super::console::Reader::open().unwrap()
    })
}

fn key_record(vk: u16, down: bool, state: u32, unit: u16, repeat: u16) -> INPUT_RECORD {
    let mut record = INPUT_RECORD {
        EventType: u16::try_from(KEY_EVENT).unwrap(),
        ..INPUT_RECORD::default()
    };
    record.Event.KeyEvent = KEY_EVENT_RECORD {
        bKeyDown: i32::from(down),
        wRepeatCount: repeat,
        wVirtualKeyCode: vk,
        wVirtualScanCode: 0,
        uChar: KEY_EVENT_RECORD_0 { UnicodeChar: unit },
        dwControlKeyState: state,
    };
    record
}

fn queue(console: &super::console::Reader, records: &[INPUT_RECORD]) {
    use std::os::windows::io::AsRawHandle as _;
    let mut written = 0;
    // SAFETY: `records` outlives the call and `written` is writable.
    let ok = unsafe {
        WriteConsoleInputW(
            console.handle().as_raw_handle(),
            records.as_ptr(),
            u32::try_from(records.len()).unwrap(),
            &raw mut written,
        )
    };
    assert_ne!(ok, 0);
    assert_eq!(written as usize, records.len());
}

#[test]
fn the_console_reader_sets_and_restores_its_modes() {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::System::Console::{
        ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT, ENABLE_MOUSE_INPUT,
        ENABLE_PROCESSED_INPUT, ENABLE_QUICK_EDIT_MODE, ENABLE_VIRTUAL_TERMINAL_INPUT,
        ENABLE_WINDOW_INPUT, SetConsoleMode,
    };
    let _serial = serial();
    let console = reader();
    let original = console.original();
    let mode = console.mode();
    let off = ENABLE_QUICK_EDIT_MODE
        | ENABLE_MOUSE_INPUT
        | ENABLE_LINE_INPUT
        | ENABLE_ECHO_INPUT
        | ENABLE_VIRTUAL_TERMINAL_INPUT;
    assert_eq!(mode & off, 0, "{mode:#x}");
    let on = ENABLE_PROCESSED_INPUT | ENABLE_WINDOW_INPUT;
    assert_eq!(mode & on, on, "{mode:#x}");
    drop(console);
    let quick = ENABLE_EXTENDED_FLAGS | ENABLE_QUICK_EDIT_MODE;
    let restored = if original & ENABLE_EXTENDED_FLAGS == 0 {
        original | quick
    } else {
        original
    };
    let again = reader();
    assert_eq!(again.original(), restored, "dropping restores the modes");

    // A mode set without the extended flags hides the QuickEdit state; dropping then turns QuickEdit on.
    let plain =
        ENABLE_PROCESSED_INPUT | ENABLE_WINDOW_INPUT | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT;
    // SAFETY: the reader's handle is a live console input handle; `again` restores its mode when dropped.
    let set = unsafe { SetConsoleMode(again.handle().as_raw_handle(), plain) };
    assert_ne!(set, 0);
    let hidden = reader();
    assert_eq!(
        hidden.original() & ENABLE_EXTENDED_FLAGS,
        0,
        "{:#x}",
        hidden.original()
    );
    drop(hidden);
    let back = reader();
    assert_eq!(back.original() & quick, quick, "{:#x}", back.original());
}

#[test]
fn the_console_reader_turns_records_into_batches() {
    use crate::chord::Modifiers;
    use crate::input::{KeyRecord, Record};
    use windows_sys::Win32::System::Console::{
        FOCUS_EVENT, LEFT_CTRL_PRESSED, RIGHT_ALT_PRESSED, SHIFT_PRESSED, WINDOW_BUFFER_SIZE_EVENT,
    };
    let _serial = serial();
    let mut console = reader();
    console.flush().unwrap();
    assert_eq!(
        console.read().unwrap(),
        crate::input::Batch {
            complete: true,
            ..Default::default()
        }
    );
    let focus = INPUT_RECORD {
        EventType: u16::try_from(FOCUS_EVENT).unwrap(),
        ..INPUT_RECORD::default()
    };
    let resize = INPUT_RECORD {
        EventType: u16::try_from(WINDOW_BUFFER_SIZE_EVENT).unwrap(),
        ..INPUT_RECORD::default()
    };
    queue(
        &console,
        &[
            key_record(0x41, true, 0, u16::from(b'a'), 1),
            // AltGr+E types the euro sign and reports Ctrl+Alt.
            key_record(0x45, true, LEFT_CTRL_PRESSED | RIGHT_ALT_PRESSED, 0x20AC, 1),
            key_record(0x28, true, SHIFT_PRESSED, 0, 3),
            key_record(0x28, false, SHIFT_PRESSED, 0, 1),
            focus,
            resize,
        ],
    );
    let batch = console.read().unwrap();
    let record = |vk, down, modifiers, text| {
        Record::Key(KeyRecord {
            vk,
            down,
            modifiers,
            text,
        })
    };
    let shift = Modifiers::SHIFT;
    assert_eq!(
        batch.records,
        [
            record(0x41, true, Modifiers::NONE, Some('a')),
            record(0x45, true, Modifiers::CTRL.union(Modifiers::ALT), Some('€')),
            record(0x28, true, shift, None),
            record(0x28, true, shift, None),
            record(0x28, true, shift, None),
            record(0x28, false, shift, None),
            Record::Focus,
        ]
    );
    assert!(batch.complete && batch.resized);

    // More than one read takes leaves the batch partial.
    let many = vec![key_record(0x41, true, 0, u16::from(b'a'), 1); super::console::BATCH + 6];
    queue(&console, &many);
    let first = console.read().unwrap();
    assert_eq!(
        (first.records.len(), first.complete),
        (super::console::BATCH, false)
    );
    let rest = console.read().unwrap();
    assert_eq!((rest.records.len(), rest.complete), (6, true));

    queue(&console, &many[..3]);
    console.flush().unwrap();
    assert!(console.read().unwrap().records.is_empty(), "flushed");
}

// ----- waiting on several handles, interrupts, console output, session handles -----

/// A new unnamed event, unsignalled.
fn event(manual: bool) -> std::os::windows::io::OwnedHandle {
    use std::os::windows::io::FromRawHandle as _;
    use windows_sys::Win32::System::Threading::CreateEventW;
    // SAFETY: an unnamed event takes no pointers but the null ones.
    let raw = unsafe { CreateEventW(std::ptr::null(), i32::from(manual), 0, std::ptr::null()) };
    assert!(!raw.is_null());
    // SAFETY: `raw` is a new event handle nothing else owns.
    unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw) }
}

fn set(event: &std::os::windows::io::OwnedHandle) {
    use std::os::windows::io::AsRawHandle as _;
    // SAFETY: `event` is a live event handle.
    let set = unsafe { windows_sys::Win32::System::Threading::SetEvent(event.as_raw_handle()) };
    assert_ne!(set, 0);
}

#[test]
fn a_wait_on_several_handles_prefers_the_earliest_and_times_out() {
    use std::os::windows::io::{
        AsHandle as _, AsRawHandle as _, BorrowedHandle, FromRawHandle as _, OwnedHandle, RawHandle,
    };
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let first = event(true);
    let second = event(false);
    let handles = [first.as_handle(), second.as_handle()];
    assert_eq!(
        crate::wait::any(&handles, Some(Duration::from_micros(100))),
        Ok(None)
    );
    set(&second);
    set(&first);
    assert_eq!(crate::wait::any(&handles, None), Ok(Some(0)));
    assert_eq!(
        crate::wait::any(&handles, Some(Duration::ZERO)),
        Ok(Some(0))
    );
    // The auto-reset event was never consumed while the first one won; alone, it is taken once.
    assert_eq!(
        crate::wait::any(&handles[1..], Some(Duration::ZERO)),
        Ok(Some(0))
    );
    assert_eq!(
        crate::wait::any(&handles[1..], Some(Duration::ZERO)),
        Ok(None)
    );

    assert!(crate::wait::any(&[], Some(Duration::ZERO)).is_err());
    let many = vec![first.as_handle(); crate::wait::MAX_HANDLES + 1];
    assert!(crate::wait::any(&many, Some(Duration::ZERO)).is_err());

    // A mutex whose owner thread ended without releasing it counts as signalled.
    // SAFETY: an unnamed, unowned mutex takes no pointers but the null ones.
    let raw = unsafe { CreateMutexW(std::ptr::null(), 0, std::ptr::null()) };
    assert!(!raw.is_null());
    // SAFETY: `raw` is a new mutex handle nothing else owns.
    let mutex = unsafe { OwnedHandle::from_raw_handle(raw) };
    let raw_mutex = mutex.as_raw_handle() as usize;
    // A plain thread, joined: a scoped thread counts as finished before its OS thread ends, and only the
    // end of the OS thread abandons the mutex.
    std::thread::spawn(move || {
        // SAFETY: the mutex outlives this thread, which is joined below.
        let borrowed = unsafe { BorrowedHandle::borrow_raw(raw_mutex as RawHandle) };
        assert_eq!(crate::wait::any(&[borrowed], None), Ok(Some(0)), "taken");
    })
    .join()
    .unwrap();
    assert_eq!(
        crate::wait::any(&[mutex.as_handle()], Some(Duration::ZERO)),
        Ok(Some(0)),
        "abandoned"
    );
}

#[test]
fn interrupts_request_shutdown_until_routed_then_count_and_signal() {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
    let _serial = serial();
    let shutdown = crate::shutdown::install().unwrap();
    bridge::reset_for_tests();
    assert!(bridge::console_notice(CTRL_C_EVENT));
    assert!(shutdown.requested(), "unrouted, Ctrl+C asks for shutdown");
    assert_eq!(shutdown.take_interrupts(), 0);

    bridge::reset_for_tests();
    let interrupt = shutdown.route_interrupts();
    assert_eq!(
        crate::wait::any(&[interrupt], Some(Duration::ZERO)),
        Ok(None)
    );
    assert!(bridge::console_notice(CTRL_C_EVENT));
    assert!(bridge::console_notice(CTRL_C_EVENT));
    assert!(bridge::console_notice(CTRL_BREAK_EVENT));
    assert!(
        !shutdown.requested(),
        "routed, Ctrl+C no longer asks for shutdown"
    );
    let handles = [shutdown.event(), interrupt];
    assert_eq!(
        crate::wait::any(&handles, Some(Duration::ZERO)),
        Ok(Some(1))
    );
    assert_eq!(
        crate::wait::any(&handles, Some(Duration::ZERO)),
        Ok(None),
        "the interrupt event resets itself"
    );
    assert_eq!(shutdown.take_interrupts(), 3);
    assert_eq!(shutdown.take_interrupts(), 0);

    // A closing window still asks for shutdown, with no session to wait for.
    assert!(bridge::console_notice(CTRL_CLOSE_EVENT));
    assert!(shutdown.requested());
    bridge::reset_for_tests();
}

/// The console output, allocating a console if the test process has none; `None` if there is still none or
/// it has no virtual-terminal processing.
fn output() -> Option<crate::console::Output> {
    crate::console::Output::open()
        .or_else(|_| {
            // SAFETY: allocating a console takes no arguments; it fails harmlessly if one exists.
            let _allocated = unsafe { windows_sys::Win32::System::Console::AllocConsole() };
            crate::console::Output::open()
        })
        .inspect_err(|error| eprintln!("skipped: no console output to test: {error}"))
        .ok()
}

#[test]
fn the_console_output_sets_and_restores_its_mode_code_page_and_title() {
    use windows_sys::Win32::System::Console::{
        ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WRAP_AT_EOL_OUTPUT,
    };
    let _serial = serial();
    let Some(probe) = output() else {
        return;
    };
    drop(probe);
    // A known starting point: no virtual-terminal processing, the US code page, a known title.
    let plain = ENABLE_PROCESSED_OUTPUT | ENABLE_WRAP_AT_EOL_OUTPUT;
    super::screen::set_mode(plain).unwrap();
    super::screen::set_code_page(437).unwrap();
    super::screen::set_title_for_tests("edbfi before").unwrap();
    let before = super::screen::current().unwrap();
    assert_eq!(before, (plain, 437, "edbfi before".to_owned()));

    let output = crate::console::Output::open().unwrap();
    assert!(
        crate::console::Output::open().is_err(),
        "only one output at a time"
    );
    let (mode, code_page, _) = super::screen::current().unwrap();
    let vt = ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
    assert_eq!(mode & vt, vt, "{mode:#x}");
    assert_eq!(mode & ENABLE_WRAP_AT_EOL_OUTPUT, ENABLE_WRAP_AT_EOL_OUTPUT);
    assert_eq!(code_page, 65_001);
    output.set_title("edbfi — dashboard").unwrap();
    assert_eq!(super::screen::current().unwrap().2, "edbfi — dashboard");
    let (width, height) = output.size().unwrap();
    assert!(width > 0 && height > 0, "{width}x{height}");

    // Armed, a panic leaves the alternate screen and restores the mode found; disarmed, it does not.
    crate::shutdown::install().unwrap();
    bridge::reset_for_tests();
    output.arm();
    assert!(output.armed());
    let _caught = std::thread::spawn(|| panic!("a deliberate test panic")).join();
    assert_eq!(
        super::screen::current().unwrap().0,
        plain,
        "the hook restored the mode"
    );
    super::screen::set_mode(mode).unwrap();
    assert!(!output.armed(), "the panic used the arming up");
    assert!(!output.disarm());
    let _caught = std::thread::spawn(|| panic!("a deliberate test panic")).join();
    assert_eq!(
        super::screen::current().unwrap().0,
        mode,
        "disarmed, the hook leaves it"
    );

    drop(output);
    assert_eq!(
        super::screen::current().unwrap(),
        before,
        "dropping restores all three"
    );
    // Standard output of a test is captured, not the console; either answer is fine, without failing.
    let _ = crate::console::stdout_is_console();
}

#[test]
fn the_process_and_stop_handles_signal_an_exit_and_a_stop_request() {
    use std::os::windows::io::BorrowedHandle;
    let _serial = serial();
    let shutdown = crate::shutdown::install().unwrap();
    bridge::reset_for_tests();
    let mut dummy = Dummy::start();
    let profile = profile(&dummy);
    let candidate = Candidate::<ReadOnly>::open(dummy.pid()).unwrap();
    let process = candidate.process_handle().unwrap();
    assert_eq!(crate::wait::any(&[process], Some(Duration::ZERO)), Ok(None));
    drop(candidate);

    let mut session = attach(&dummy, &profile);
    let quiet = |session: &Session<Installed>| {
        let handles: [BorrowedHandle<'_>; 2] = [
            session.stop_handle().unwrap(),
            session.process_handle().unwrap(),
        ];
        crate::wait::any(&handles, Some(Duration::ZERO))
    };
    assert_eq!(quiet(&session), Ok(None));

    // After a shutdown request a publication stores zero, whatever it asked for.
    session.publish(mask(&[0])).unwrap();
    assert_eq!(dummy.ask("alpha"), "alpha 1");
    shutdown.request();
    session.publish(mask(&[0])).unwrap();
    assert_eq!(session.published(), DesiredMask::ZERO);
    assert_eq!(dummy.ask("alpha"), "alpha 0");
    bridge::reset_for_tests();

    let outcomes = request_stop(profile.profile(), Some(dummy.pid())).unwrap();
    assert_eq!(outcomes, [(dummy.pid(), StopOutcome::Requested)]);
    assert_eq!(quiet(&session), Ok(Some(0)), "the stop event is signalled");
    let detached = session.restore(VerifyRestore::Sites).unwrap();

    dummy.child.kill().unwrap();
    dummy.child.wait().unwrap();
    assert_eq!(
        crate::wait::any(
            &[detached.process_handle().unwrap()],
            Some(Duration::from_secs(5))
        ),
        Ok(Some(0)),
        "the process handle is signalled once the game exits"
    );
}

/// Discovery that sees only one process, so other games on the machine never interfere.
struct Only(u32);

impl crate::runtime::Discover for Only {
    fn find(&self) -> Result<Vec<u32>, crate::patch::OsError> {
        Ok(vec![self.0])
    }

    fn open(&self, pid: u32) -> Result<Candidate<Write>, crate::patch::OsError> {
        Candidate::<Write>::open(pid)
    }
}

/// Runs the runtime's due steps until `until` holds, for at most ten seconds.
fn drive(runtime: &mut crate::runtime::Runtime, until: impl Fn(&crate::runtime::Runtime) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !until(runtime) {
        assert!(Instant::now() < deadline, "{runtime:?}");
        match runtime.due() {
            Some(at) => {
                std::thread::sleep(at.saturating_duration_since(Instant::now()));
                runtime.step();
            }
            None => panic!("nothing is due: {runtime:?}"),
        }
    }
}

#[test]
fn the_runtime_attaches_to_a_real_process_and_hears_its_stop_and_exit() {
    use crate::lifecycle::{Finish, Phase, RuntimeCommand};
    use crate::runtime::{Done, Options, Runtime};
    let _serial = serial();
    let _shutdown = crate::shutdown::install().unwrap();
    bridge::reset_for_tests();
    let dir = crate::testdir::TestDir::new("win-runtime");
    let locations = crate::location::Locations {
        settings: dir.path().join("config"),
        logs: dir.path().join("logs"),
    };
    let mut dummy = Dummy::start();
    let profile = profile(&dummy);
    let start = |dummy: &Dummy| {
        Runtime::start(
            profile.clone(),
            Box::new(Only(dummy.pid())),
            Ok(locations.clone()),
            crate::journal::Journal::memory(),
            Options::default(),
        )
        .0
    };

    // Attach, publish, then a stop request from another process: woken by the stop event, it restores.
    let mut runtime = start(&dummy);
    drive(&mut runtime, |runtime| runtime.phase().is_attached());
    runtime.command(RuntimeCommand::Publish(mask(&[0])));
    assert_eq!(dummy.ask("alpha"), "alpha 1");
    let quiet = |runtime: &Runtime| {
        let handles = [
            runtime.stop_handle().unwrap(),
            runtime.process_handle().unwrap(),
        ];
        crate::wait::any(&handles, Some(Duration::ZERO))
    };
    assert_eq!(quiet(&runtime), Ok(None));
    let outcomes = request_stop(profile.profile(), Some(dummy.pid())).unwrap();
    assert_eq!(outcomes, [(dummy.pid(), StopOutcome::Requested)]);
    assert_eq!(
        quiet(&runtime),
        Ok(Some(0)),
        "the stop event wakes the owner"
    );
    runtime.stop();
    drive(&mut runtime, |runtime| runtime.done().is_some());
    assert_eq!(runtime.done(), Some(&Done::Finished(Finish::Restored)));
    let image = Candidate::<ReadOnly>::open(dummy.pid())
        .map(|candidate| identify(candidate, &profile).unwrap().image())
        .unwrap();
    assert_eq!(sites(&dummy, &profile, image), originals(&profile));
    assert_eq!(dummy.ask("alpha"), "alpha 0");
    drop(runtime);

    // Attach again; the game exits: woken by its process handle, the runtime waits for the next one.
    let mut runtime = start(&dummy);
    drive(&mut runtime, |runtime| runtime.phase().is_attached());
    dummy.child.kill().unwrap();
    dummy.child.wait().unwrap();
    let process = runtime.process_handle().unwrap();
    assert_eq!(
        crate::wait::any(&[process], Some(Duration::from_secs(5))),
        Ok(Some(0))
    );
    runtime.exited();
    assert_eq!(*runtime.phase(), Phase::Waiting);
    assert_eq!(runtime.game(), None);
    assert!(runtime.process_handle().is_none());
}
