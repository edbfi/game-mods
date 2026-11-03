// SPDX-License-Identifier: AGPL-3.0-only
//! Closing a trainer's console window, as its close button does. Windows then gives the trainer a few
//! seconds before it ends the process, and a trainer connected to a game must put the game back first.
//!
//! Each trainer runs in a console of its own. The test attaches to that console only long enough to find its
//! window, then posts the window `WM_CLOSE`. They need the Windows Console Host, as CI has: with Windows
//! Terminal as the default terminal there is no such window to close. `cargo test` without target filters
//! builds the `dummy_game` and `dummy_trainer` examples they start; `EDBFI_TRAINER_EXE` points them at
//! another build of `metro-trainer`.
#![cfg(windows)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers shared by tests; a failed call is a test failure"
)]

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use edbfi_trainer::profile::{ValidProfile, dummy};
use edbfi_trainer::session::{Candidate, ReadOnly, SiteStatus, identify};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, FreeConsole, GetConsoleWindow,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

/// `CREATE_NEW_CONSOLE`: the child gets a console, and a window, of its own.
const NEW_CONSOLE: u32 = 0x0000_0010;

/// How a process ended by a console control event exits: `STATUS_CONTROL_C_EXIT`.
const CONTROL_EXIT: i32 = 0xC000_013A_u32 as i32;

/// Both tests attach this process to another console for a moment.
static SERIAL: Mutex<()> = Mutex::new(());

fn examples() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent()
        .and_then(Path::parent)
        .unwrap()
        .join("examples")
}

fn example(name: &str) -> PathBuf {
    let path = examples().join(format!("{name}.exe"));
    assert!(
        path.exists(),
        "{} is missing: run `cargo test --workspace` without target filters so the examples are built",
        path.display()
    );
    path
}

fn trainer_exe() -> PathBuf {
    std::env::var_os("EDBFI_TRAINER_EXE").map_or_else(
        || PathBuf::from(env!("CARGO_BIN_EXE_metro-trainer")),
        PathBuf::from,
    )
}

/// An empty folder that stands in for the user's profile, removed afterwards.
struct Home(PathBuf);

impl Home {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("edbfi-close-{}-{name}", std::process::id()));
        let _removed = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn log(&self, slug: &str) -> PathBuf {
        self.0
            .join("local")
            .join("edbfi")
            .join(slug)
            .join("trainer")
            .join("trainer.log")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

/// A trainer in a console of its own, its output read as it comes so it never blocks on a full pipe.
struct Trainer {
    child: Child,
    output: Option<JoinHandle<String>>,
}

impl Trainer {
    fn start(exe: &Path, args: &[&str], home: &Home, env: &[(&str, &str)]) -> Self {
        let mut child = Command::new(exe)
            .args(args)
            .env("APPDATA", home.0.join("roaming"))
            .env("LOCALAPPDATA", home.0.join("local"))
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(NEW_CONSOLE)
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let output = std::thread::spawn(move || {
            let mut text = String::new();
            let _read = stdout.read_to_string(&mut text);
            text
        });
        Self {
            child,
            output: Some(output),
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Waits up to `timeout` for the trainer to end.
    fn ended(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// What it printed; ends it first if it still runs.
    fn output(&mut self) -> String {
        let _killed = self.child.kill();
        let _waited = self.child.wait();
        self.output
            .take()
            .map(|reader| reader.join().unwrap())
            .unwrap_or_default()
    }
}

impl Drop for Trainer {
    fn drop(&mut self) {
        let _killed = self.child.kill();
        let _waited = self.child.wait();
    }
}

/// Waits up to `timeout` for `ready`.
fn until(timeout: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while !ready() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// The window of the console process `pid` runs in, found by attaching to that console for a moment.
fn console_window(pid: u32) -> HWND {
    let mut window = std::ptr::null_mut();
    let found = until(Duration::from_secs(10), || {
        // SAFETY: detaching from this process's console, if it has one, takes no pointers. Output already
        // goes through handles that stay valid.
        let _freed = unsafe { FreeConsole() };
        // SAFETY: attaching to another process's console takes no pointers.
        if unsafe { AttachConsole(pid) } != 0 {
            // SAFETY: takes no arguments.
            window = unsafe { GetConsoleWindow() };
            // SAFETY: as above; this process must not be attached when the window closes.
            let _freed = unsafe { FreeConsole() };
        }
        !window.is_null()
    });
    // SAFETY: rejoining the console this process started in, if any, takes no pointers.
    let _rejoined = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    assert!(found, "process {pid} has no console window to close");
    window
}

/// Closes the console window of process `pid`, as its close button does.
fn close_console(pid: u32) {
    let window = console_window(pid);
    // SAFETY: `window` is a console window; `WM_CLOSE` takes no pointers.
    let posted = unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
    assert_ne!(posted, 0, "WM_CLOSE was not posted");
}

#[test]
fn closing_a_waiting_trainer_ends_it_promptly() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = Home::new("waiting");
    // This test's own process is never the game, so the trainer waits for as long as it runs.
    let me = std::process::id().to_string();
    let mut trainer = Trainer::start(&trainer_exe(), &["--pid", &me], &home, &[]);
    // The log opens once the close handler is in place.
    let log = home.log("metro-2033-redux");
    assert!(
        until(Duration::from_secs(10), || std::fs::read_to_string(&log)
            .is_ok_and(|text| text.contains("=== metro-trainer "))),
        "the trainer never started:\n{}",
        trainer.output()
    );
    let started = Instant::now();
    close_console(trainer.pid());
    let status = trainer.ended(Duration::from_secs(10));
    assert!(
        status.is_some(),
        "the trainer still runs after its console closed:\n{}",
        trainer.output()
    );
    let status = status.unwrap();
    assert!(
        matches!(status.code(), Some(0 | CONTROL_EXIT)),
        "{status:?} after {:?}:\n{}",
        started.elapsed(),
        trainer.output()
    );
}

/// The dummy game, running.
struct Game {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    layout: String,
}

impl Game {
    fn start() -> Self {
        let mut child = Command::new(example("dummy_game"))
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
            layout: layout.trim().to_owned(),
        }
    }

    fn ask(&mut self, command: &str) -> String {
        writeln!(self.input, "{command}").unwrap();
        self.input.flush().unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        line.trim().to_owned()
    }

    /// Each site's state, read through a handle that cannot write.
    fn sites(&self, profile: &ValidProfile<'static>) -> Vec<SiteStatus> {
        let candidate = Candidate::<ReadOnly>::open(self.child.id()).unwrap();
        match identify(candidate, profile) {
            Ok(_) => vec![SiteStatus::Verified; profile.profile().sites.len()],
            Err(rejected) => rejected
                .error
                .sites()
                .unwrap_or_else(|| panic!("{:?}", rejected.error))
                .iter()
                .map(|check| check.status)
                .collect(),
        }
    }
}

impl Drop for Game {
    fn drop(&mut self) {
        let _killed = self.child.kill();
        let _waited = self.child.wait();
    }
}

#[test]
fn closing_a_connected_trainer_puts_the_game_back_first() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = Home::new("connected");
    let mut game = Game::start();
    let file = std::fs::read(example("dummy_game")).unwrap();
    let profile = ValidProfile::new(dummy::profile(&file, &game.layout).unwrap()).unwrap();
    assert!(
        game.sites(&profile)
            .iter()
            .all(|status| *status == SiteStatus::Verified)
    );
    let path = example("dummy_game");
    let mut trainer = Trainer::start(
        &example("dummy_trainer"),
        &["--pid", &game.child.id().to_string()],
        &home,
        &[
            ("EDBFI_DUMMY_GAME", path.to_str().unwrap()),
            ("EDBFI_DUMMY_LAYOUT", &game.layout),
        ],
    );
    assert!(
        until(Duration::from_secs(30), || game
            .sites(&profile)
            .iter()
            .all(|status| *status != SiteStatus::Verified)),
        "the trainer never hooked every site:\n{}",
        trainer.output()
    );
    let log = home.log(profile.profile().slug.game);
    assert!(
        until(Duration::from_secs(10), || std::fs::read_to_string(&log)
            .is_ok_and(|text| text.contains("hooked the game"))),
        "the trainer never finished hooking:\n{}",
        trainer.output()
    );

    close_console(trainer.pid());
    let status = trainer.ended(Duration::from_secs(10));
    assert!(
        status.is_some(),
        "the trainer still runs after its console closed:\n{}",
        trainer.output()
    );
    let status = status.unwrap();
    assert_eq!(
        game.sites(&profile),
        vec![SiteStatus::Verified; profile.profile().sites.len()],
        "every site is back, and the code section hashes as the file's"
    );
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("restored every hooked site"), "{text}");
    assert!(
        matches!(status.code(), Some(0 | CONTROL_EXIT)),
        "{status:?}:\n{text}"
    );
    // The game runs on, unhooked.
    assert_eq!(game.ask("alpha"), "alpha 0");
}
