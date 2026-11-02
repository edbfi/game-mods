// SPDX-License-Identifier: AGPL-3.0-only
//! The Metro dashboard against a simulated runtime, for review on any terminal without the game.
//!
//! `cargo run -p metro-2033-redux-trainer --example demo [-- --ascii] [-- --mono | --truecolour]`
//!
//! Nothing touches a process or writes a file: publications are acknowledged at once and saves succeed in
//! memory. The simulation has its own keys, which the trainer does not have:
//!
//! - Ctrl+G starts the game, or exits it once it runs.
//! - Ctrl+E fails the connection; Ctrl+T fails a restore and leaves hooks behind.
//! - Ctrl+S makes the settings file invalid.
//!
//! With output redirected it plays a short script and prints the plain report instead.

use std::collections::VecDeque;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::{cursor, execute, terminal};
use edbfi_trainer::chord::{Key, Modifiers};
use edbfi_trainer::control::Ack;
use edbfi_trainer::input::{Command, ConsoleKey, Press};
use edbfi_trainer::lifecycle::{
    Failure, Finish, Phase, RuntimeCommand, RuntimeEvent, SettingsProblem, VerifyStage,
};
use edbfi_trainer::profile::{SiteId, ValidProfile};
use edbfi_trainer::recovery::RecoveryAction;
use edbfi_trainer::settings::{Saved, Settings};
use edbfi_tui::app::{App, Effect, Msg, Paths, Startup};
use edbfi_tui::plain::Report;
use edbfi_tui::theme::{Appearance, Colour, Glyphs};
use metro_2033_redux_trainer::profile::METRO;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// How long each simulated step takes, so every phase is visible.
const STEP: Duration = Duration::from_millis(400);

/// The simulated runtime: a phase, a publication counter and the next timed step.
struct Sim {
    phase: Phase,
    revision: u64,
    next: Option<Instant>,
    quitting: bool,
    done: Option<Finish>,
    /// Whether a terminal shows the dashboard; plain output never gets escape sequences.
    terminal: bool,
}

impl Sim {
    fn enter(&mut self, phase: Phase, queue: &mut VecDeque<Msg>) {
        self.phase = phase.clone();
        self.next = matches!(
            phase,
            Phase::Verifying { .. } | Phase::Installing | Phase::Restoring
        )
        .then(|| Instant::now() + STEP);
        queue.push_back(Msg::Runtime(RuntimeEvent::Phase(phase)));
    }

    /// Advances a timed phase.
    fn tick(&mut self, queue: &mut VecDeque<Msg>) {
        self.next = None;
        let next = match &self.phase {
            Phase::Verifying { stage } => match VerifyStage::ALL.iter().position(|s| s == stage) {
                Some(at) if at + 1 < VerifyStage::ALL.len() => Phase::Verifying {
                    stage: VerifyStage::ALL[at + 1],
                },
                _ => Phase::Installing,
            },
            Phase::Installing => Phase::Attached {
                attach_duration: Duration::from_millis(42),
            },
            Phase::Restoring if self.quitting => {
                self.done = Some(Finish::Restored);
                Phase::Finished(Finish::Restored)
            }
            Phase::Restoring => Phase::Waiting,
            _ => return,
        };
        self.enter(next, queue);
    }

    fn effect(
        &mut self,
        effect: Effect,
        app: &App<'_>,
        queue: &mut VecDeque<Msg>,
    ) -> io::Result<()> {
        match effect {
            Effect::Runtime(RuntimeCommand::Publish(mask)) if self.phase.is_attached() => {
                self.revision += 1;
                queue.push_back(Msg::Runtime(RuntimeEvent::Ack(Ack::simulated(
                    self.revision,
                    mask,
                ))));
            }
            Effect::Runtime(RuntimeCommand::Publish(_)) => {}
            Effect::Runtime(RuntimeCommand::Quit) => {
                self.quitting = true;
                if self.phase.is_attached() {
                    self.enter(Phase::Restoring, queue);
                } else {
                    self.done = Some(Finish::Untouched);
                    self.enter(Phase::Finished(Finish::Untouched), queue);
                }
            }
            Effect::Runtime(
                command @ (RuntimeCommand::Retry
                | RuntimeCommand::ResetSettings
                | RuntimeCommand::SessionOnly),
            ) => {
                if let Phase::Settings(_) = self.phase {
                    let profile = profile()?;
                    let path = (command != RuntimeCommand::SessionOnly).then(settings_path);
                    queue.push_back(Msg::Loaded {
                        settings: Settings::defaults(&profile),
                        path,
                    });
                }
                self.enter(Phase::Waiting, queue);
            }
            Effect::Save => queue.push_back(Msg::Saved(Ok(Saved::Written { external: None }))),
            Effect::Title if self.terminal => {
                execute!(io::stdout(), terminal::SetTitle(app.title()))?
            }
            Effect::Title => {}
            Effect::Bindings | Effect::Chime(_) => {}
        }
        Ok(())
    }

    /// The simulation's own keys; returns whether `key` was one.
    fn key(&mut self, key: &KeyEvent, queue: &mut VecDeque<Msg>) -> io::Result<bool> {
        if key.modifiers != KeyModifiers::CONTROL {
            return Ok(false);
        }
        let failure = |recovery, message: &str, unrestored| {
            Phase::Failed(Failure {
                recovery,
                message: message.into(),
                unrestored,
            })
        };
        match key.code {
            KeyCode::Char('g') => match self.phase {
                Phase::Waiting => self.enter(
                    Phase::Verifying {
                        stage: VerifyStage::Executable,
                    },
                    queue,
                ),
                // The game exited: nothing to restore, so back to waiting.
                Phase::Verifying { .. } | Phase::Attached { .. } => {
                    self.enter(Phase::Waiting, queue);
                }
                _ => {}
            },
            KeyCode::Char('e') => self.enter(
                failure(
                    RecoveryAction::CloseDuplicates,
                    "Two copies of metro.exe are running (PIDs 4120 and 7788).",
                    Vec::new(),
                ),
                queue,
            ),
            KeyCode::Char('t') => self.enter(
                failure(
                    RecoveryAction::RestartGame,
                    "Restoring the game's code failed: WriteProcessMemory was denied (OS error 5).",
                    vec![SiteId(3), SiteId(7)],
                ),
                queue,
            ),
            KeyCode::Char('s') => {
                let profile = profile()?;
                let error =
                    Settings::parse(&profile, b"schema = 1\n[trainer]\nstart_paused = maybe\n")
                        .err()
                        .ok_or_else(|| io::Error::other("the sample settings parsed"))?;
                self.enter(
                    Phase::Settings(SettingsProblem::Invalid {
                        path: settings_path(),
                        error,
                    }),
                    queue,
                );
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

fn profile() -> io::Result<ValidProfile<'static>> {
    ValidProfile::new(&METRO).map_err(io::Error::other)
}

fn settings_path() -> PathBuf {
    PathBuf::from("demo/settings.toml (never written)")
}

/// What the trainer's input layer would deliver for a terminal key: a hotkey when it matches a binding and
/// no capture is open, otherwise the key itself.
fn command(app: &App<'_>, key: &KeyEvent) -> Option<Command> {
    let mut modifiers = Modifiers::NONE;
    for (flag, modifier) in [
        (KeyModifiers::CONTROL, Modifiers::CTRL),
        (KeyModifiers::ALT, Modifiers::ALT),
        (KeyModifiers::SHIFT, Modifiers::SHIFT),
    ] {
        if key.modifiers.contains(flag) {
            modifiers = modifiers.union(modifier);
        }
    }
    let console = match key.code {
        KeyCode::Char('c') if modifiers == Modifiers::CTRL => return Some(Command::Interrupt),
        KeyCode::Up => ConsoleKey::Up,
        KeyCode::Down => ConsoleKey::Down,
        KeyCode::Left => ConsoleKey::Left,
        KeyCode::Right => ConsoleKey::Right,
        KeyCode::Tab => ConsoleKey::Tab,
        KeyCode::BackTab => {
            modifiers = Modifiers::SHIFT;
            ConsoleKey::Tab
        }
        KeyCode::Enter => ConsoleKey::Enter,
        KeyCode::Esc => ConsoleKey::Escape,
        KeyCode::Backspace => ConsoleKey::Backspace,
        KeyCode::Home => ConsoleKey::Key(Key::HOME),
        KeyCode::End => ConsoleKey::Key(Key::END),
        KeyCode::PageUp => ConsoleKey::Key(Key::PAGE_UP),
        KeyCode::PageDown => ConsoleKey::Key(Key::PAGE_DOWN),
        KeyCode::Delete => ConsoleKey::Key(Key::DELETE),
        KeyCode::F(n) => ConsoleKey::Key(Key::function(n)?),
        KeyCode::Char(' ') => ConsoleKey::Key(Key::SPACE),
        KeyCode::Char(c) if c.is_ascii_alphabetic() => {
            ConsoleKey::Key(Key::letter(c.to_ascii_uppercase() as u8)?)
        }
        KeyCode::Char(c) if c.is_ascii_digit() => ConsoleKey::Key(Key::digit(c as u8 - b'0')?),
        KeyCode::Char(c) => ConsoleKey::Char(c),
        _ => return None,
    };
    let press = Press {
        key: console,
        modifiers,
    };
    if !app.capturing()
        && let Some(chord) = press.chord()
        && let Some(target) = app.settings().bindings.holder(chord)
    {
        return Some(Command::Hotkey(target));
    }
    Some(Command::Press(press))
}

fn appearance() -> Appearance {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|arg| arg == flag);
    let colour = if has("--mono") || std::env::var_os("NO_COLOR").is_some() {
        Colour::Mono
    } else if has("--truecolour")
        || std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit")
    {
        Colour::TrueColour
    } else {
        Colour::Ansi16
    };
    let glyphs = if has("--ascii") {
        Glyphs::Ascii
    } else {
        Glyphs::Unicode
    };
    Appearance { colour, glyphs }
}

fn startup(size: (u16, u16)) -> Startup {
    let mut settings =
        Settings::defaults(&ValidProfile::new(&METRO).unwrap_or_else(|_| unreachable!()));
    settings.sounds = false;
    Startup {
        settings,
        paths: Paths {
            settings: Some(settings_path()),
            log: None,
        },
        phase: Phase::Waiting,
        appearance: appearance(),
        size,
    }
}

fn new_sim(terminal: bool) -> Sim {
    Sim {
        terminal,
        phase: Phase::Waiting,
        revision: 0,
        next: None,
        quitting: false,
        done: None,
    }
}

fn drain(app: &mut App<'_>, sim: &mut Sim, queue: &mut VecDeque<Msg>) -> io::Result<()> {
    while let Some(msg) = queue.pop_front() {
        for effect in app.update(msg) {
            sim.effect(effect, app, queue)?;
        }
    }
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "the demo reads terminal events itself; the trainer reads its console through the input layer"
)]
fn interactive() -> io::Result<Option<Finish>> {
    let mut out = io::stdout();
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let size = terminal.size()?;
    let mut app = App::new(profile()?, startup((size.width, size.height)));
    let mut sim = new_sim(true);
    let mut queue = VecDeque::new();
    queue.push_back(Msg::Resize {
        width: size.width,
        height: size.height,
    });
    while sim.done.is_none() || !queue.is_empty() {
        drain(&mut app, &mut sim, &mut queue)?;
        if app.take_redraw() {
            terminal.draw(|frame| frame.render_widget(&app, frame.area()))?;
        }
        if sim.done.is_some() {
            std::thread::sleep(STEP);
            break;
        }
        // With nothing scheduled the loop sleeps until the next key: an idle dashboard never wakes.
        let timeout = sim.next.map_or(Duration::from_secs(3600), |at| {
            at.saturating_duration_since(Instant::now())
        });
        if !event::poll(timeout)? {
            sim.tick(&mut queue);
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if !sim.key(&key, &mut queue)?
                    && let Some(command) = command(&app, &key)
                {
                    queue.push_back(Msg::Input(command));
                }
            }
            Event::Resize(width, height) => queue.push_back(Msg::Resize { width, height }),
            _ => {}
        }
    }
    execute!(out, terminal::SetTitle(""))?;
    Ok(sim.done)
}

/// Plays a short script and prints the plain report, for redirected output.
fn scripted() -> io::Result<()> {
    let mut app = App::new(profile()?, startup((80, 24)));
    let mut sim = new_sim(false);
    let mut report = Report::new();
    let mut out = io::stdout().lock();
    let profile = profile()?;
    let ammo = profile
        .toggle_id("unlimited_ammo")
        .ok_or_else(|| io::Error::other("no ammo toggle"))?;
    let steps = [
        None,
        Some(Msg::Input(Command::Hotkey(
            edbfi_trainer::settings::Target::Toggle(ammo),
        ))),
        Some(Msg::Runtime(RuntimeEvent::Phase(Phase::Verifying {
            stage: VerifyStage::Executable,
        }))),
        Some(Msg::Input(Command::Hotkey(
            edbfi_trainer::settings::Target::Master,
        ))),
    ];
    for step in steps {
        let mut queue = VecDeque::new();
        if let Some(Msg::Runtime(RuntimeEvent::Phase(phase))) = step {
            sim.enter(phase, &mut queue);
            while sim.next.is_some() {
                sim.tick(&mut queue);
                drain(&mut app, &mut sim, &mut queue)?;
            }
        } else if let Some(msg) = step {
            queue.push_back(msg);
        }
        drain(&mut app, &mut sim, &mut queue)?;
        for line in report.lines(&app) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

fn main() -> io::Result<()> {
    if !io::stdout().is_terminal() {
        return scripted();
    }
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), terminal::EnterAlternateScreen, cursor::Hide)?;
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen, cursor::Show);
        previous(info);
    }));
    let result = interactive();
    terminal::disable_raw_mode()?;
    execute!(io::stdout(), terminal::LeaveAlternateScreen, cursor::Show)?;
    match result? {
        Some(Finish::Restored) => println!("The game's code is restored and no cheats remain."),
        Some(Finish::Untouched) | None => println!("The game was not changed."),
    }
    Ok(())
}
