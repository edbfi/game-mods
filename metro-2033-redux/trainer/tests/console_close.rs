// SPDX-License-Identifier: AGPL-3.0-only
//! Closing a trainer's terminal while it runs. Windows then gives the trainer a few seconds before it ends
//! the process, and a trainer connected to a game must put the game back first.
//!
//! Each trainer runs in a pseudoconsole that the test hosts, as a Windows Terminal tab hosts it; closing the
//! pseudoconsole is what closing the tab does, and sends the trainer the same close event a console window's
//! close button does. Its output is a console, so the trainer draws its dashboard. `cargo test` without
//! target filters builds the `dummy_game` and `dummy_trainer` examples these tests start;
//! `EDBFI_TRAINER_EXE` points them at another build of `metro-trainer`.
#![cfg(windows)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers shared by tests; a failed call is a test failure"
)]

use std::ffi::{OsStr, OsString, c_void};
use std::fs::File;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use edbfi_trainer::profile::{ValidProfile, dummy};
use edbfi_trainer::session::{Candidate, ReadOnly, SiteStatus, identify};
use windows_sys::Win32::Foundation::{FALSE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Console::{COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

/// How a process ended by a console's close event exits: `STATUS_CONTROL_C_EXIT`.
const CONTROL_EXIT: u32 = 0xC000_013A;

/// One trainer at a time.
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

    /// `APPDATA` and `LOCALAPPDATA` inside it.
    fn env(&self) -> Vec<(&'static str, OsString)> {
        vec![
            ("APPDATA", self.0.join("roaming").into_os_string()),
            ("LOCALAPPDATA", self.0.join("local").into_os_string()),
        ]
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

/// A pipe's read and write ends.
fn pipe() -> (File, File) {
    let (mut read, mut write): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: both out-pointers are writable; no attributes, the default size.
    let made = unsafe { CreatePipe(&raw mut read, &raw mut write, std::ptr::null(), 0) };
    assert_ne!(made, 0, "CreatePipe: {}", std::io::Error::last_os_error());
    // SAFETY: `read` is a fresh pipe handle that nothing else owns.
    let read = unsafe { File::from_raw_handle(read) };
    // SAFETY: as above.
    let write = unsafe { File::from_raw_handle(write) };
    (read, write)
}

/// This process's environment with `overrides`, as a sorted `CreateProcessW` block.
fn environment(overrides: &[(&str, OsString)]) -> Vec<u16> {
    let mut vars: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(name, _)| {
            !overrides
                .iter()
                .any(|(other, _)| name.eq_ignore_ascii_case(other))
        })
        .chain(
            overrides
                .iter()
                .map(|(name, value)| (OsString::from(name), value.clone())),
        )
        .collect();
    vars.sort_by_key(|(name, _)| name.to_ascii_uppercase());
    let mut block = Vec::new();
    for (name, value) in vars {
        block.extend(name.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

/// A process hosted in a pseudoconsole, its output read as it comes so the console never blocks on it.
struct Hosted {
    console: Option<HPCON>,
    process: OwnedHandle,
    _input: File,
    output: Option<JoinHandle<Vec<u8>>>,
}

impl Hosted {
    fn start(exe: &Path, args: &[&str], env: &[(&str, OsString)]) -> Self {
        let mut line: Vec<u16> = OsStr::new("\"")
            .encode_wide()
            .chain(exe.as_os_str().encode_wide())
            .chain(OsStr::new("\"").encode_wide())
            .collect();
        for arg in args {
            assert!(!arg.contains([' ', '"']), "{arg:?} needs quoting");
            line.push(u16::from(b' '));
            line.extend(OsStr::new(arg).encode_wide());
        }
        line.push(0);
        let env = environment(env);

        let (input_read, input) = pipe();
        let (mut output, output_write) = pipe();
        let mut console: HPCON = 0;
        // SAFETY: both pipe ends are live; `console` is writable. The console duplicates the handles.
        let created = unsafe {
            CreatePseudoConsole(
                COORD { X: 100, Y: 30 },
                input_read.as_raw_handle(),
                output_write.as_raw_handle(),
                0,
                &raw mut console,
            )
        };
        assert!(created >= 0, "CreatePseudoConsole: {created:#x}");
        drop((input_read, output_write));

        let mut size = 0;
        // SAFETY: asks only for the size of a one-attribute list, which it reports by failing.
        let _sized =
            unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &raw mut size) };
        let mut storage = vec![0u64; size.div_ceil(8)];
        let attributes = storage.as_mut_ptr().cast::<c_void>();
        // SAFETY: `attributes` points at `size` writable, aligned bytes.
        let initialised =
            unsafe { InitializeProcThreadAttributeList(attributes, 1, 0, &raw mut size) };
        assert_ne!(initialised, 0);
        // SAFETY: the list is initialised; the pseudoconsole attribute takes the console handle itself as
        // its value.
        let updated = unsafe {
            UpdateProcThreadAttribute(
                attributes,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                console as *const c_void,
                size_of::<HPCON>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert_ne!(updated, 0);

        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        // Invalid standard handles: the child takes the console's, not this process's redirected ones.
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        startup.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
        startup.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
        startup.lpAttributeList = attributes;
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: `line` and `env` are NUL-terminated and live; `startup` is an extended start-up record
        // whose attribute list is initialised; `info` is writable. No handles are inherited.
        let spawned = unsafe {
            CreateProcessW(
                std::ptr::null(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                FALSE,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                env.as_ptr().cast(),
                std::ptr::null(),
                &raw const startup.StartupInfo,
                &raw mut info,
            )
        };
        let error = std::io::Error::last_os_error();
        // SAFETY: the list was initialised above and is no longer needed.
        unsafe { DeleteProcThreadAttributeList(attributes) };
        assert_ne!(spawned, 0, "CreateProcessW {}: {error}", exe.display());
        // SAFETY: a fresh thread handle that nothing else owns.
        drop(unsafe { OwnedHandle::from_raw_handle(info.hThread) });
        // SAFETY: a fresh process handle that nothing else owns.
        let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };

        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _read = output.read_to_end(&mut bytes);
            bytes
        });
        Self {
            console: Some(console),
            process,
            _input: input,
            output: Some(reader),
        }
    }

    /// Waits up to `timeout` for the process to end, and returns its exit code.
    fn ended(&self, timeout: Duration) -> Option<u32> {
        let millis = u32::try_from(timeout.as_millis()).unwrap();
        // SAFETY: a live process handle.
        if unsafe { WaitForSingleObject(self.process.as_raw_handle(), millis) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0;
        // SAFETY: a live process handle; `code` is writable.
        let read = unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &raw mut code) };
        assert_ne!(read, 0);
        Some(code)
    }

    /// Closes the pseudoconsole, as closing a terminal tab does. Closing waits for the console's processes
    /// on some versions of Windows, so it runs on a thread of its own.
    fn close(&mut self) {
        if let Some(console) = self.console.take() {
            // SAFETY: a pseudoconsole this value created and closes once.
            std::thread::spawn(move || unsafe { ClosePseudoConsole(console) });
        }
    }

    /// What it drew, escapes made visible. Ends it first if it still runs.
    fn output(&mut self) -> String {
        self.kill();
        self.close();
        let bytes = self
            .output
            .take()
            .map(|reader| reader.join().unwrap())
            .unwrap_or_default();
        String::from_utf8_lossy(&bytes).replace('\u{1b}', "\\e")
    }

    fn kill(&self) {
        // SAFETY: a live process handle; ending a process that already ended fails harmlessly.
        let _ended = unsafe { TerminateProcess(self.process.as_raw_handle(), 1) };
    }
}

impl Drop for Hosted {
    fn drop(&mut self) {
        self.kill();
        self.close();
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

fn logged(log: &Path, needle: &str) -> bool {
    std::fs::read_to_string(log).is_ok_and(|text| text.contains(needle))
}

#[test]
fn closing_a_waiting_trainer_ends_it_promptly() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = Home::new("waiting");
    // This test's own process is never the game, so the trainer waits for as long as it runs.
    let me = std::process::id().to_string();
    let mut trainer = Hosted::start(&trainer_exe(), &["--pid", &me], &home.env());
    // The log opens once the close handler is in place.
    let log = home.log("metro-2033-redux");
    assert!(
        until(Duration::from_secs(10), || logged(
            &log,
            "=== metro-trainer "
        )),
        "the trainer never started:\n{}",
        trainer.output()
    );
    let started = Instant::now();
    trainer.close();
    let code = trainer.ended(Duration::from_secs(10));
    assert!(
        matches!(code, Some(0 | CONTROL_EXIT)),
        "{code:x?} after {:?}:\n{}",
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
    let path = example("dummy_game");
    let file = std::fs::read(&path).unwrap();
    let profile = ValidProfile::new(dummy::profile(&file, &game.layout).unwrap()).unwrap();
    let verified = vec![SiteStatus::Verified; profile.profile().sites.len()];
    assert_eq!(game.sites(&profile), verified);
    let mut env = home.env();
    env.push(("EDBFI_DUMMY_GAME", path.into_os_string()));
    env.push(("EDBFI_DUMMY_LAYOUT", OsString::from(&game.layout)));
    let mut trainer = Hosted::start(
        &example("dummy_trainer"),
        &["--pid", &game.child.id().to_string()],
        &env,
    );
    let log = home.log(profile.profile().slug.game);
    assert!(
        until(Duration::from_secs(30), || logged(&log, "hooked the game")),
        "the trainer never hooked the game:\n{}",
        trainer.output()
    );
    assert!(
        game.sites(&profile)
            .iter()
            .all(|status| *status != SiteStatus::Verified),
        "every site is hooked"
    );

    trainer.close();
    let code = trainer.ended(Duration::from_secs(10));
    assert!(
        code.is_some(),
        "the trainer still runs after its terminal closed:\n{}",
        trainer.output()
    );
    assert_eq!(
        game.sites(&profile),
        verified,
        "every site is back, and the code section hashes as the file's"
    );
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("restored every hooked site"), "{text}");
    assert!(matches!(code, Some(0 | CONTROL_EXIT)), "{code:x?}:\n{text}");
    // The game runs on, unhooked.
    assert_eq!(game.ask("alpha"), "alpha 0");
}
