// SPDX-License-Identifier: AGPL-3.0-only
//! Runs the dashboard: routes each wake to the runtime or the input arbiter, carries out the app's effects,
//! and draws once when something visible changed.
//!
//! [`Driver`] is the same on every platform. The runtime is a [`Backend`] and the platform a [`Host`], so
//! tests drive it with pretend ones; on Windows, `windows::run` waits on the real handles and feeds each
//! [`Wake`] here.
//!
//! The input gate follows the phase: it closes while the runtime writes or restores the game's code, and
//! reopens through an epoch barrier once it is done, so no key pressed meanwhile acts afterwards. A retry
//! closes it too. When the runtime finishes, the gate shuts for good.

#[cfg(test)]
mod tests;
#[cfg(windows)]
pub mod windows;

use std::collections::VecDeque;

use edbfi_trainer::input::{Arbiter, Batch, Command, Epoch, Probe, Wakes};
use edbfi_trainer::lifecycle::{Phase, RuntimeCommand, RuntimeEvent};
use edbfi_trainer::patch::OsError;
use edbfi_trainer::runtime::Runtime;
use edbfi_trainer::settings::{Saved, Settings, StoreError};

use crate::app::{App, Chime, Effect, Msg};
use crate::plain::Report;

/// The runtime, as the driver uses it.
pub trait Backend {
    /// Carries out a dashboard command.
    fn command(&mut self, command: RuntimeCommand);
    /// Saves the settings.
    ///
    /// # Errors
    ///
    /// The [`StoreError`] if they cannot be saved.
    fn save(&mut self, settings: &Settings) -> Result<Saved, StoreError>;
    /// Runs the step that is due.
    fn step(&mut self);
    /// Another trainer process asked this one to stop.
    fn stop(&mut self);
    /// The console is closing, the session is ending or something panicked.
    fn shutdown(&mut self);
    /// The game process may have exited.
    fn exited(&mut self);
    /// Takes what happened since the last call.
    fn events(&mut self) -> Vec<RuntimeEvent>;
    /// The attached game process.
    fn game(&self) -> Option<u32>;
    /// Whether the run has ended.
    fn finished(&self) -> bool;
}

impl Backend for Runtime {
    fn command(&mut self, command: RuntimeCommand) {
        Self::command(self, command);
    }

    fn save(&mut self, settings: &Settings) -> Result<Saved, StoreError> {
        Self::save(self, settings)
    }

    fn step(&mut self) {
        Self::step(self);
    }

    fn stop(&mut self) {
        Self::stop(self);
    }

    fn shutdown(&mut self) {
        Self::shutdown(self);
    }

    fn exited(&mut self) {
        Self::exited(self);
    }

    fn events(&mut self) -> Vec<RuntimeEvent> {
        Self::events(self)
    }

    fn game(&self) -> Option<u32> {
        Self::game(self)
    }

    fn finished(&self) -> bool {
        self.done().is_some()
    }
}

/// The platform, as the driver uses it.
pub trait Host {
    /// What the arbiter asks about the keyboard and windows.
    type Probe: Probe;

    /// The probe.
    fn probe(&self) -> &Self::Probe;
    /// Asks the platform to acknowledge `epoch` after every keyboard wake already queued. Returns `false` if
    /// it cannot, so the driver acknowledges it at once: with no keyboard notices there is nothing to wait
    /// for.
    fn barrier(&mut self, epoch: Epoch) -> bool;
    /// Reads the console's waiting records without blocking.
    ///
    /// # Errors
    ///
    /// The [`OsError`] if the console can no longer be read.
    fn read_console(&mut self) -> Result<Batch, OsError>;
    /// Discards the console's waiting records.
    ///
    /// # Errors
    ///
    /// The [`OsError`] if the console can no longer be used.
    fn flush_console(&mut self) -> Result<(), OsError>;
    /// The terminal's size in cells, width then height.
    fn size(&mut self) -> (u16, u16);
    /// Plays a chime.
    fn chime(&mut self, chime: Chime);
    /// Sets the window title.
    fn title(&mut self, title: &str);
    /// Draws the dashboard.
    ///
    /// # Errors
    ///
    /// The I/O error if the terminal can no longer be drawn; the driver switches to plain output.
    fn draw(&mut self, app: &App<'_>) -> std::io::Result<()>;
    /// Leaves the dashboard's screen for good; everything after is plain lines.
    fn leave(&mut self);
    /// Prints plain lines.
    fn print(&mut self, lines: &[String]);
}

/// What woke the owner thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// The console is closing, the session is ending or something panicked.
    Shutdown,
    /// Ctrl+C or Ctrl+Break was pressed this many times.
    Interrupts(u32),
    /// Another trainer process asked this one to stop.
    Stop,
    /// The game process is signalled.
    Exited,
    /// The keyboard and foreground notices.
    Keyboard(Wakes),
    /// Console records are waiting.
    Console,
    /// The runtime's next step is due.
    Due,
}

/// The dashboard and the runtime, joined. See the module documentation.
#[derive(Debug)]
pub struct Driver<'p, B, H> {
    app: App<'p>,
    backend: B,
    host: H,
    arbiter: Arbiter,
    report: Report,
    queue: VecDeque<Msg>,
    /// Whether the gate is closed for a phase or a retry; the next settled batch reopens it.
    closed: bool,
    /// Whether the console is still read.
    console: bool,
    /// Whether the dashboard's screen was left for plain lines.
    left: bool,
}

impl<'p, B: Backend, H: Host> Driver<'p, B, H> {
    /// Joins `app` and `backend`, sets the title, opens the input gate and shows the first frame.
    pub fn new(mut app: App<'p>, backend: B, mut host: H, console: bool) -> Self {
        let arbiter = Arbiter::new(&app.settings().bindings);
        host.title(app.title());
        let effects = app.take_effects();
        let mut driver = Self {
            app,
            backend,
            host,
            arbiter,
            report: Report::new(),
            queue: VecDeque::new(),
            closed: true,
            console,
            left: false,
        };
        if !console {
            driver.lose_console();
        }
        driver.effects(effects);
        driver.settle();
        driver
    }

    /// The app.
    #[must_use]
    pub const fn app(&self) -> &App<'p> {
        &self.app
    }

    /// The runtime.
    #[must_use]
    pub const fn backend(&self) -> &B {
        &self.backend
    }

    /// The runtime, to finish it after a panic left the driver unusable.
    pub const fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// The platform.
    #[must_use]
    pub const fn host(&self) -> &H {
        &self.host
    }

    /// The input arbiter.
    #[must_use]
    pub const fn arbiter(&self) -> &Arbiter {
        &self.arbiter
    }

    /// Whether the console is still read; stop waiting on it once it is not.
    #[must_use]
    pub const fn reads_console(&self) -> bool {
        self.console
    }

    /// Whether the run has ended.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.backend.finished() && self.queue.is_empty()
    }

    /// Takes the driver apart.
    pub fn into_parts(self) -> (App<'p>, B, H) {
        (self.app, self.backend, self.host)
    }

    /// Handles one wake, then everything it led to, and draws.
    pub fn wake(&mut self, wake: Wake) {
        match wake {
            // The restore runs before anything is drawn: a console write can stall (a selection parks it),
            // and a closing window gives the restore only the operating system's budget.
            Wake::Shutdown => {
                self.backend.shutdown();
                self.backend.step();
            }
            Wake::Interrupts(count) => {
                // A third press adds nothing: the second already confirms the quit.
                for _ in 0..count.min(2) {
                    self.queue.push_back(Msg::Input(Command::Interrupt));
                }
            }
            Wake::Stop => {
                self.backend.stop();
                self.backend.step();
            }
            Wake::Exited => self.backend.exited(),
            Wake::Due => self.backend.step(),
            Wake::Keyboard(wakes) => self.keyboard(wakes),
            Wake::Console => self.console(),
        }
        self.settle();
    }

    fn keyboard(&mut self, wakes: Wakes) {
        // Merged notices: a key held when the focus or the keyboards changed is primed before any press
        // is judged, so a chord pressed elsewhere cannot act once the game comes to the front.
        if wakes.devices {
            self.arbiter.devices(self.host.probe());
        }
        if wakes.foreground {
            self.arbiter.foreground(self.host.probe());
            // A console window entering the front is established by one read, even of nothing.
            self.console();
        }
        if wakes.keyboard {
            let commands = self.arbiter.physical(self.host.probe());
            self.commands(commands);
        }
        if let Some(epoch) = wakes.barrier {
            self.acknowledged(epoch);
        }
    }

    fn console(&mut self) {
        if !self.console {
            return;
        }
        match self.host.read_console() {
            Ok(batch) => {
                if batch.resized {
                    let (width, height) = self.host.size();
                    self.queue.push_back(Msg::Resize { width, height });
                }
                let commands = self.arbiter.console(&batch, self.host.probe());
                self.commands(commands);
            }
            Err(_) => self.lose_console(),
        }
    }

    fn commands(&mut self, commands: Vec<Command>) {
        self.queue.extend(commands.into_iter().map(Msg::Input));
    }

    /// The console can no longer be read or drawn: its commands stop, and output becomes plain lines.
    fn lose_console(&mut self) {
        self.console = false;
        self.arbiter.console_failed();
        if !self.left {
            self.left = true;
            self.host.leave();
        }
        if !self.app.plain() {
            self.queue.push_back(Msg::ConsoleLost);
        }
    }

    /// The platform acknowledged `epoch`: if the gate waits for it, flush the console and open.
    fn acknowledged(&mut self, epoch: Epoch) {
        let Some(flush) = self.arbiter.barrier(epoch) else {
            return;
        };
        if self.console && self.host.flush_console().is_err() {
            self.lose_console();
        }
        self.arbiter.open(flush, self.host.probe());
    }

    fn barrier(&mut self, epoch: Option<Epoch>) {
        if let Some(epoch) = epoch
            && !self.host.barrier(epoch)
        {
            self.acknowledged(epoch);
        }
    }

    /// Runs every queued message and runtime event, keeps the gate in step with the phase, and draws.
    fn settle(&mut self) {
        loop {
            self.queue
                .extend(self.backend.events().into_iter().map(Msg::Runtime));
            if let Some(msg) = self.queue.pop_front() {
                let effects = self.app.update(msg);
                self.effects(effects);
                continue;
            }
            // Reopening the gate can lose the console, which queues a message of its own.
            self.gate();
            if self.queue.is_empty() {
                break;
            }
        }
        self.draw();
    }

    /// The input epoch changed: keys queued under the old one never act. Interrupts and runtime events stay.
    fn invalidate(&mut self) {
        self.queue
            .retain(|msg| !matches!(msg, Msg::Input(Command::Hotkey(_) | Command::Press(_))));
    }

    /// Carries out one update's effects: runtime commands first, then the rest, then saves.
    fn effects(&mut self, effects: Vec<Effect>) {
        for effect in &effects {
            if let Effect::Runtime(command) = *effect {
                if command == RuntimeCommand::Retry {
                    // Whatever the retry leads to, keys pressed before it never act after it.
                    self.arbiter.close();
                    self.closed = true;
                    self.invalidate();
                }
                self.backend.command(command);
            }
        }
        for effect in &effects {
            match *effect {
                Effect::Bindings => {
                    self.invalidate();
                    let epoch = self
                        .arbiter
                        .rebind(&self.app.settings().bindings, self.app.capturing());
                    self.barrier(epoch);
                }
                Effect::Chime(chime) => self.host.chime(chime),
                Effect::Title => self.host.title(self.app.title()),
                Effect::Runtime(_) | Effect::Save => {}
            }
        }
        if effects.contains(&Effect::Save) {
            let saved = self.backend.save(self.app.settings());
            self.queue.push_back(Msg::Saved(saved));
        }
    }

    /// Closes the gate while code is written or restored, shuts it once the run ends, and reopens it after.
    fn gate(&mut self) {
        self.arbiter.set_game(self.backend.game());
        if self.backend.finished() {
            self.arbiter.shut();
            self.closed = true;
            return;
        }
        let busy = matches!(
            self.app.phase(),
            Phase::Installing | Phase::Restoring | Phase::Finished(_)
        );
        if busy && !self.closed {
            self.arbiter.close();
            self.closed = true;
        } else if !busy && self.closed {
            self.closed = false;
            let epoch = self.arbiter.reopen();
            self.barrier(epoch);
        }
    }

    fn draw(&mut self) {
        if !self.app.take_redraw() {
            return;
        }
        if !self.app.plain() {
            match self.host.draw(&self.app) {
                Ok(()) => return,
                Err(_) => {
                    self.lose_console();
                    // The app hears the console is lost, then the whole state is printed as lines.
                    self.settle();
                    return;
                }
            }
        }
        let lines = self.report.lines(&self.app);
        if !lines.is_empty() {
            self.host.print(&lines);
        }
    }
}
