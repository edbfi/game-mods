// SPDX-License-Identifier: AGPL-3.0-only
//! The whole lifecycle against a real process: the `dummy_game` example, attached, toggled, paused,
//! restored and attached again, plus the close, session-end and panic paths. Run by `cargo test`, which
//! builds the example first.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use windows_sys::Win32::System::Console::CTRL_CLOSE_EVENT;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SendMessageW, WM_ENDSESSION, WM_QUERYENDSESSION,
};

use crate::addr::{Rva, Va};
use crate::aob::{Pattern, Token};
use crate::control::{ControlBit, ControlSet, DesiredMask};
use crate::identity::Sha256Digest;
use crate::patch::Process as _;
use crate::pe::PeImage;
use crate::profile::synthetic::SYNTHETIC;
use crate::profile::{
    Class, Context, Game, GateEffect, HookKind, Identity, PlayerLayout, Profile, ReloadKind, Site,
    SiteId, TextSection, ValidProfile,
};
use crate::session::{
    Candidate, Installed, ReadOnly, Session, StopOutcome, TextCheck, TrainerError, VerifyRestore,
    Write, identify, request_stop,
};

use super::bridge;
use super::process::WinGame;

/// The bridge, its shutdown event and its lease slot are process-wide, so these tests take turns.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    layout: Vec<(String, u32)>,
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
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let layout = line
            .trim()
            .strip_prefix("layout ")
            .unwrap()
            .split(' ')
            .map(|pair| {
                let (key, value) = pair.split_once('=').unwrap();
                let value = u32::from_str_radix(value.trim_start_matches("0x"), 16).unwrap();
                (key.to_owned(), value)
            })
            .collect();
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

    fn rva(&self, key: &str) -> Rva {
        Rva::new(self.layout.iter().find(|(k, _)| k == key).unwrap().1)
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

fn leak<T>(items: Vec<T>) -> &'static [T] {
    Box::leak(items.into_boxed_slice())
}

/// A site at the first match of `marker` in `text`, `skip` bytes in, `len` long, with the 16 bytes from the
/// marker as its signature.
fn site(
    text: &[u8],
    text_rva: u32,
    marker: &[u8],
    skip: usize,
    len: usize,
    id: u8,
    kind: HookKind,
) -> Site {
    let at = text
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap_or_else(|| panic!("marker {marker:02X?} not in the dummy's code"));
    let tokens = leak(
        text[at..at + 16]
            .iter()
            .map(|&byte| Token::Byte(byte))
            .collect(),
    );
    Site {
        id: SiteId(id),
        rva: Rva::new(text_rva + (at + skip) as u32),
        original: leak(text[at + skip..at + skip + len].to_vec()),
        context: Context {
            rva: Rva::new(text_rva + at as u32),
            aob: Pattern::new(tokens).unwrap(),
        },
        kind,
    }
}

/// A profile of the dummy: its file and code fingerprints, its three sites, and its globals.
fn profile(dummy: &Dummy) -> ValidProfile<'static> {
    let file = std::fs::read(dummy_path()).unwrap();
    let pe = PeImage::parse(&file).unwrap();
    let section = pe.section(".text").unwrap();
    let text = section.loaded(&file).unwrap();
    let base = section.rva().get();
    let alpha = site(
        &text,
        base,
        &[
            0x48, 0x89, 0x5C, 0x24, 0x08, 0x57, 0x48, 0x83, 0xEC, 0x20, 0x31, 0xC0, 0xB9, 0x01,
            0x00, 0xED,
        ],
        0,
        5,
        1,
        HookKind::Gate {
            bit: ControlBit::new(0),
            effect: GateEffect::ReturnTrue,
        },
    );
    let drain_marker = [0xB9, 0x02, 0x00, 0xED, 0x5E, 0xE8];
    let at = text.windows(6).position(|w| w == drain_marker).unwrap();
    let rel = i32::from_le_bytes(text[at + 6..at + 10].try_into().unwrap());
    let callee = Rva::new((i64::from(base) + (at + 10) as i64 + i64::from(rel)) as u32);
    let drain = site(
        &text,
        base,
        &drain_marker,
        5,
        5,
        2,
        HookKind::Gate {
            bit: ControlBit::new(1),
            effect: GateEffect::SkipBatteryCall { callee },
        },
    );
    let fire = site(
        &text,
        base,
        &[
            0x8D, 0x50, 0xFF, 0xFF, 0x93, 0x60, 0x13, 0x00, 0x00, 0xB8, 0x03, 0x00, 0xED, 0x5E,
        ],
        0,
        9,
        3,
        HookKind::NoReload(ReloadKind::DirectCallbackRcx),
    );
    let classes = leak(vec![
        Class {
            interned_ids: leak(vec![dummy.rva("red")]),
            ..SYNTHETIC.classes[0]
        },
        Class {
            interned_ids: leak(vec![dummy.rva("blue"), dummy.rva("blue_alias")]),
            ..SYNTHETIC.classes[1]
        },
    ]);
    let profile: &'static Profile = Box::leak(Box::new(Profile {
        game: Game {
            name: "Dummy Game",
            exe: "dummy_game.exe",
        },
        identity: Identity {
            disk_sha256: Sha256Digest::of(&file),
            text: TextSection {
                rva: section.rva(),
                size: section.virtual_size(),
                sha256: Sha256Digest::of(&text),
            },
        },
        player: PlayerLayout {
            manager: dummy.rva("manager"),
            ..SYNTHETIC.player
        },
        classes,
        sites: leak(vec![alpha, drain, fire]),
        derived: &[],
        ..SYNTHETIC
    }));
    ValidProfile::new(profile).unwrap()
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

    let mut session = attach(&dummy, &profile);
    assert_eq!(session.image(), image);
    assert!(
        sites(&dummy, &profile, image)
            .iter()
            .all(|site| site[0] == 0xE9)
    );
    // Installed with every cheat off: the game behaves as before.
    assert_eq!(dummy.ask("alpha"), "alpha 0");
    assert_eq!(dummy.ask("drain"), "battery 98");
    assert_eq!(dummy.ask("fire"), "rounds 28");

    session.publish(mask(&[0, 1, 2]));
    assert_eq!(dummy.ask("alpha"), "alpha 1");
    assert_eq!(dummy.ask("drain"), "battery 98");
    assert_eq!(dummy.ask("fire"), "rounds 28");
    // Only the red class is kept; a blue selection leaves the red weapon spending rounds.
    session.publish(mask(&[3]));
    assert_eq!(dummy.ask("fire"), "rounds 27");
    // Paused.
    session.publish(DesiredMask::ZERO);
    assert_eq!(dummy.ask("alpha"), "alpha 0");
    assert_eq!(dummy.ask("drain"), "battery 97");

    // Another trainer is refused while this one owns the game.
    let other = identify(Candidate::<Write>::open(dummy.pid()).unwrap(), &profile).unwrap();
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
    session.publish(mask(&[2]));
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
    session.publish(mask(&[2]));
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
    session.publish(mask(&[0]));
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
    session.publish(mask(&[0]));
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
    session.publish(mask(&[0]));
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
    session.publish(mask(&[0]));
    assert_eq!(dummy.ask("alpha"), "alpha 1");
    let _caught = std::thread::spawn(|| panic!("a deliberate test panic")).join();
    assert_eq!(dummy.ask("alpha"), "alpha 0");
    session.restore(VerifyRestore::Sites).unwrap();
}
