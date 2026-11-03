// SPDX-License-Identifier: AGPL-3.0-only
//! The dashboard on a Windows console: the owner thread's one blocking wait, and the platform the
//! [`Driver`] draws and reads through.
//!
//! [`run`] installs the shutdown bridge before anything touches a game, routes Ctrl+C to the dashboard,
//! starts the keyboard notices and opens the console. Then it waits on, in priority order: the shutdown
//! request, Ctrl+C, the game's stop event, the game process, the keyboard notices and the console input, with
//! the runtime's next step as the only timeout. With nothing due and nothing pressed it sleeps indefinitely:
//! an idle dashboard never wakes. A due step runs after every wake, so a flood of keys cannot hold it back.
//!
//! A panic on the owner thread is caught outside the dashboard: the runtime alone restores the game, then
//! the panic's summary is printed.

use std::io::{self, Stdout, Write as _};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use edbfi_trainer::console::{self, Output};
use edbfi_trainer::input::platform::{Console, Keyboard, System};
use edbfi_trainer::input::{Batch, Epoch};
use edbfi_trainer::journal::{EntryKind, Event, Journal};
use edbfi_trainer::location::Locations;
use edbfi_trainer::patch::OsError;
use edbfi_trainer::profile::{Profile, ValidProfile};
use edbfi_trainer::runtime::{self, Done, Processes, Runtime};
use edbfi_trainer::session::VerifyRestore;
use edbfi_trainer::shutdown::{self, Shutdown};
use edbfi_trainer::sound::{self, Tone};
use edbfi_trainer::wait;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::style::{Attribute, SetAttribute};
use ratatui::crossterm::{cursor, execute, terminal};

use super::{Driver, Host, Wake};
use crate::app::{App, Chime, Paths, Startup};
use crate::outcome::Outcome;
use crate::theme::Appearance;

/// How the trainer runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Connect only to this process.
    pub pid: Option<u32>,
    /// Also hash the whole code section after each restore.
    pub verify: VerifyRestore,
    /// Print plain lines instead of drawing the dashboard.
    pub plain: bool,
    /// The colours and glyphs; `None` picks them from the environment.
    pub appearance: Option<Appearance>,
}

/// The most steps a runtime finishing without the dashboard runs; a restore is one step.
const FINISH_STEPS: usize = 64;

/// The platform: the keyboard notices, the console, and the screen while the dashboard is drawn.
struct Win {
    probe: System,
    keyboard: Option<Keyboard>,
    console: Option<Console>,
    output: Option<Output>,
    terminal: Option<Terminal<CrosstermBackend<Stdout>>>,
}

impl std::fmt::Debug for Win {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Win")
            .field("keyboard", &self.keyboard.is_some())
            .field("console", &self.console.is_some())
            .field("screen", &self.terminal.is_some())
            .finish_non_exhaustive()
    }
}

impl Win {
    /// Enters the alternate screen with the cursor hidden.
    fn enter(output: &Output) -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
        let mut stdout = io::stdout();
        execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;
        output.arm();
        let entered = Terminal::new(CrosstermBackend::new(stdout)).and_then(|mut terminal| {
            terminal.clear()?;
            Ok(terminal)
        });
        if entered.is_err() {
            Self::exit(output);
        }
        entered
    }

    /// Leaves the alternate screen with the cursor shown and the attributes reset, unless a panic already
    /// did.
    fn exit(output: &Output) {
        if output.disarm() {
            let _left = execute!(
                io::stdout(),
                SetAttribute(Attribute::Reset),
                cursor::Show,
                terminal::LeaveAlternateScreen
            );
        }
    }
}

impl Host for Win {
    type Probe = System;

    fn probe(&self) -> &System {
        &self.probe
    }

    fn barrier(&mut self, epoch: Epoch) -> bool {
        self.keyboard
            .is_some_and(|keyboard| keyboard.barrier(epoch).is_ok())
    }

    fn read_console(&mut self) -> Result<Batch, OsError> {
        match &mut self.console {
            Some(console) => console.read(),
            None => Err(OsError::new("ReadConsoleInputW", 6)),
        }
    }

    fn flush_console(&mut self) -> Result<(), OsError> {
        match &mut self.console {
            Some(console) => console.flush(),
            None => Ok(()),
        }
    }

    fn size(&mut self) -> (u16, u16) {
        self.output
            .as_ref()
            .and_then(|output| output.size().ok())
            .unwrap_or((80, 24))
    }

    fn chime(&mut self, chime: Chime) {
        sound::play(match chime {
            Chime::Rise => Tone::Rise,
            Chime::Fall => Tone::Fall,
        });
    }

    fn title(&mut self, title: &str) {
        if let Some(output) = &self.output {
            let _set = output.set_title(title);
        }
    }

    fn draw(&mut self, app: &App<'_>) -> io::Result<()> {
        let terminal = self
            .terminal
            .as_mut()
            .ok_or_else(|| io::Error::other("the dashboard's screen is closed"))?;
        terminal.draw(|frame| frame.render_widget(app, frame.area()))?;
        Ok(())
    }

    fn leave(&mut self) {
        if let Some(terminal) = self.terminal.take() {
            drop(terminal);
            if let Some(output) = &self.output {
                Self::exit(output);
            }
        }
        // A console that failed is not read again; dropping it restores its modes.
        self.console = None;
    }

    fn print(&mut self, lines: &[String]) {
        let mut stdout = io::stdout().lock();
        for line in lines {
            let _written = writeln!(stdout, "{line}");
        }
        let _flushed = stdout.flush();
    }
}

/// What a handle in the wait stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Shutdown,
    Interrupt,
    Stop,
    Exited,
    Keyboard,
    Console,
}

/// Runs the trainer for `profile` until it finishes, then prints the summary. `header` starts each log
/// session, such as the trainer's name and version.
pub fn run(profile: &'static Profile, header: &str, options: Options) -> Outcome {
    // Before anything can touch a game: a session prepared without the bridge is invisible to a closing
    // window, logoff or panic.
    let shutdown = match shutdown::install() {
        Ok(shutdown) => shutdown,
        Err(error) => return not_started(format!("cannot watch for shutdown: {error}")),
    };
    let interrupts = shutdown.route_interrupts();
    let valid = match ValidProfile::new(profile) {
        Ok(valid) => valid,
        Err(error) => return not_started(format!("the game profile is invalid: {error:?}")),
    };
    let locations = Locations::from_env(profile.slug);
    let journal = match &locations {
        Ok(locations) => Journal::open(&locations.logs, header),
        Err(_) => Journal::memory(),
    };
    let (mut runtime, started) = Runtime::start(
        valid.clone(),
        Box::new(Processes(profile)),
        locations,
        journal,
        runtime::Options {
            pid: options.pid,
            verify: options.verify,
        },
    );
    let keyboard = match Keyboard::start(&shutdown) {
        Ok(keyboard) => Some(keyboard),
        Err(error) => {
            runtime.record(Event::error(
                EntryKind::Session,
                format!("game hotkeys cannot work: Windows refused keyboard notices ({error})"),
            ));
            None
        }
    };
    // Plain or redirected output leaves the console's modes, code page and title alone.
    let (output, console_reader, terminal) = if !options.plain && console::stdout_is_console() {
        match screen() {
            Ok(screen) => screen,
            Err(warning) => {
                runtime.record(Event::warning(EntryKind::Session, warning));
                (None, None, None)
            }
        }
    } else {
        (None, None, None)
    };
    let size = output
        .as_ref()
        .and_then(|output| output.size().ok())
        .unwrap_or((80, 24));
    let appearance = options
        .appearance
        .unwrap_or_else(|| Appearance::detect(|name| std::env::var_os(name)));
    let app = App::new(
        valid,
        Startup {
            settings: started.settings,
            paths: Paths {
                settings: started.settings_path,
                log: started.log_path,
            },
            phase: started.phase,
            appearance,
            size,
        },
    );
    let reads = console_reader.is_some();
    let host = Win {
        probe: System,
        keyboard,
        console: console_reader,
        output,
        terminal,
    };
    let mut driver = Driver::new(app, runtime, host, reads);
    let pumped = catch_unwind(AssertUnwindSafe(|| pump(&mut driver, shutdown, interrupts)));
    let (_app, mut runtime, mut host) = driver.into_parts();
    let fault = match pumped {
        Ok(fault) => fault,
        Err(payload) => Some(format!(
            "the trainer panicked: {}",
            panic_message(&*payload)
        )),
    };
    if let Some(fault) = &fault {
        runtime.record(Event::error(EntryKind::Session, fault.clone()));
    }
    if fault.is_some() || runtime.done().is_none() {
        // The dashboard may be broken: the runtime finishes on its own.
        finish(&mut runtime);
    }
    host.leave();
    let log = runtime.journal().path().map(ToOwned::to_owned);
    let done = runtime.done().cloned().unwrap_or_else(|| Done::HooksLeft {
        sites: Vec::new(),
        message: "the trainer stopped before it could restore the game".to_owned(),
    });
    let outcome = match fault {
        Some(fault) => Outcome::Faulted { fault, done },
        None => Outcome::Ran(done),
    };
    drop(runtime);
    host.print(&outcome.summary(log.as_deref()));
    // Dropping the platform restores the console's modes and title.
    drop(host);
    outcome
}

/// A panic payload's message, when it has one.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic without a message".to_owned())
}

/// The console output, input and screen for the dashboard, or why it prints lines instead.
#[expect(clippy::type_complexity, reason = "one tuple, destructured once")]
fn screen() -> Result<
    (
        Option<Output>,
        Option<Console>,
        Option<Terminal<CrosstermBackend<Stdout>>>,
    ),
    String,
> {
    let output = Output::open()
        .map_err(|error| format!("cannot draw on this console, printing lines instead: {error}"))?;
    let console = Console::open()
        .map_err(|error| format!("cannot read the console, printing lines instead: {error}"))?;
    let terminal = Win::enter(&output)
        .map_err(|error| format!("cannot draw the dashboard, printing lines instead: {error}"))?;
    Ok((Some(output), Some(console), Some(terminal)))
}

fn not_started(message: String) -> Outcome {
    let outcome = Outcome::NotStarted(message);
    for line in outcome.summary(None) {
        eprintln!("{line}");
    }
    outcome
}

/// Restores and ends a runtime the dashboard can no longer drive.
fn finish(runtime: &mut Runtime) {
    runtime.shutdown();
    for _ in 0..FINISH_STEPS {
        if runtime.done().is_some() {
            break;
        }
        runtime.step();
    }
    let _events = runtime.events();
}

/// The owner thread's loop: one wait, then the wake, then any due step, until the runtime finishes. Returns
/// why it had to shut down by itself, if it did.
fn pump(
    driver: &mut Driver<'_, Runtime, Win>,
    shutdown: Shutdown,
    interrupts: std::os::windows::io::BorrowedHandle<'static>,
) -> Option<String> {
    // A shutdown request and a stop request stay signalled; each is handled once.
    let mut shut = false;
    let mut stopped = false;
    while !driver.finished() {
        let woke = {
            let runtime = driver.backend();
            let host = driver.host();
            let mut sources = Vec::with_capacity(6);
            let mut handles = Vec::with_capacity(6);
            if !shut {
                sources.push(Source::Shutdown);
                handles.push(shutdown.event());
            }
            sources.push(Source::Interrupt);
            handles.push(interrupts);
            if !stopped && let Some(stop) = runtime.stop_handle() {
                sources.push(Source::Stop);
                handles.push(stop);
            }
            if let Some(process) = runtime.process_handle() {
                sources.push(Source::Exited);
                handles.push(process);
            }
            if let Some(keyboard) = &host.keyboard {
                sources.push(Source::Keyboard);
                handles.push(keyboard.event());
            }
            if driver.reads_console()
                && let Some(console) = &host.console
            {
                sources.push(Source::Console);
                handles.push(console.handle());
            }
            let timeout = runtime
                .due()
                .map(|at| at.saturating_duration_since(Instant::now()));
            wait::any(&handles, timeout).map(|woke| woke.map(|index| sources[index]))
        };
        let wake = match woke {
            Ok(Some(Source::Shutdown)) => {
                shut = true;
                Some(Wake::Shutdown)
            }
            Ok(Some(Source::Interrupt)) => match shutdown.take_interrupts() {
                0 => None,
                count => Some(Wake::Interrupts(count)),
            },
            Ok(Some(Source::Stop)) => {
                stopped = true;
                Some(Wake::Stop)
            }
            Ok(Some(Source::Exited)) => Some(Wake::Exited),
            Ok(Some(Source::Keyboard)) => driver
                .host()
                .keyboard
                .map(|keyboard| Wake::Keyboard(keyboard.take())),
            Ok(Some(Source::Console)) => Some(Wake::Console),
            Ok(None) => None,
            Err(error) => {
                // Without its wait the owner cannot hear anything: restore and exit at once.
                driver.wake(Wake::Shutdown);
                finish(driver.backend_mut());
                return Some(format!("cannot wait for events: {error}"));
            }
        };
        if let Some(wake) = wake {
            driver.wake(wake);
        }
        if driver
            .backend()
            .due()
            .is_some_and(|at| at <= Instant::now())
        {
            driver.wake(Wake::Due);
        }
        if shut && !driver.finished() && driver.backend().due().is_none() {
            // Shutting down with nothing left to step: whatever the runtime holds, the run is over.
            finish(driver.backend_mut());
            // Let the dashboard show the end before it closes.
            driver.wake(Wake::Due);
        }
    }
    None
}
