// SPDX-License-Identifier: AGPL-3.0-only
//! Which key events act, and which are ignored.
//!
//! Keys reach the trainer by two paths. On the game path, Windows wakes the trainer on every keyboard event
//! anywhere (Raw Input), and the trainer asks what is physically held at that moment: a wake on its own
//! carries no authority. On the console path, the trainer reads its own console's key records. [`Arbiter`]
//! combines the two so one physical press acts at most once:
//!
//! - Each key has one hold, shared by both paths, from its first press until a [`Probe`] confirms it is up.
//!   A hold that acted, or was down when the trainer last primed, never acts again: not on autorepeat, not
//!   when its modifiers change, and not when the focus moves between the console and the game.
//! - A hotkey acts in the game on a fresh press of its key, with exactly its modifiers held, no Windows key
//!   held and the game's window in front.
//! - A hotkey acts in the console from its key's first record in a complete batch, unless a focus record,
//!   a change of modifiers while the key is held, or its release in the same batch cancels it. A classic
//!   console window in front needs no more, once one batch has been read with it in front (records read
//!   on the way in may be older than the focus change). Anywhere else, such as a Windows Terminal tab, the
//!   key must still be physically down when the batch is read, because the records cannot say where they
//!   came from; a tap released before the trainer reads it is lost. Nothing in the console acts while the
//!   game's window is in front.
//! - Keys that are not hotkeys become [`Press`]es for the dashboard; while a binding is being captured,
//!   every key does, so a bound chord can be captured too.
//! - The gate closes while game code is written or restored and while bindings change. It reopens only
//!   through an epoch barrier: the platform acknowledges the epoch after every wake it sent before, the
//!   console's waiting records are discarded, and every held key is primed.
#![forbid(unsafe_code)]

use crate::chord::{Chord, Key, Modifiers};
use crate::settings::{Bindings, Target};

use super::{Command, ConsoleKey, Press};

/// One generation of input: a barrier for an older epoch is stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Epoch(u64);

impl Epoch {
    /// The epoch as a number, to send through the platform.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The epoch a platform acknowledgement carries.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

/// The keyboard and windows as they are now.
pub trait Probe {
    /// Whether the key with virtual-key code `vk` is physically down.
    fn down(&self, vk: u8) -> bool;
    /// Ctrl, Alt and Shift as physically held, or `None` while either Windows key is held.
    fn modifiers(&self) -> Option<Modifiers>;
    /// The process owning the foreground window.
    fn foreground(&self) -> Option<u32>;
    /// Where the trainer's console window is.
    fn console(&self) -> ConsoleFocus;
}

/// Where the trainer's console window is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConsoleFocus {
    /// A classic console window, in front.
    Front,
    /// A classic console window, behind another window.
    Behind,
    /// No window the trainer can see, as in a Windows Terminal tab.
    Hidden,
}

/// One console input record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Record {
    /// A key went down (or repeated) or up.
    Key(KeyRecord),
    /// The console gained or lost the focus.
    Focus,
}

/// A key record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyRecord {
    /// The virtual-key code.
    pub vk: u16,
    /// Down (including autorepeat) or up.
    pub down: bool,
    /// Ctrl, Alt and Shift as the record reports them. AltGr reports Ctrl+Alt.
    pub modifiers: Modifiers,
    /// The character the key types, if any.
    pub text: Option<char>,
}

/// The records of one read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Batch {
    /// The records, oldest first.
    pub records: Vec<Record>,
    /// Whether nothing more was waiting after this read. A partial batch authorises no hotkey, since a focus
    /// change may still be queued behind it.
    pub complete: bool,
    /// Whether the console buffer changed size.
    pub resized: bool,
}

/// Proof that a barrier was acknowledged: discard the console's waiting records, then pass it to
/// [`Arbiter::open`].
#[must_use = "the gate stays closed until the console is flushed and this is passed to Arbiter::open"]
#[derive(Debug, PartialEq, Eq)]
pub struct Flush(Epoch);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gate {
    Open,
    Closed,
    Reopening(Epoch),
    Shut,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Hold {
    #[default]
    Up,
    Down {
        /// The press acted or was primed; only its release rearms the key.
        spent: bool,
        /// The console has seen a record of this press; later records are autorepeat.
        recorded: bool,
    },
}

/// A hotkey seen in the current batch, acting when the batch ends unless something cancels it first.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    vk: u8,
    modifiers: Modifiers,
    target: Target,
    released: bool,
}

/// Decides which key events become [`Command`]s. See the module documentation for the rules.
#[derive(Clone, Debug)]
pub struct Arbiter {
    bound: Vec<(Chord, Target)>,
    capturing: bool,
    game: Option<u32>,
    gate: Gate,
    epoch: Epoch,
    holds: [Hold; 256],
    /// The console window was in front for the last batch, with no focus change since.
    established: bool,
    reader: bool,
}

const VK_C: u8 = 0x43;

impl Arbiter {
    /// An arbiter for `bindings`, with its gate closed. Open it with [`Arbiter::reopen`].
    #[must_use]
    pub fn new(bindings: &Bindings) -> Self {
        Self {
            bound: Self::bound(bindings),
            capturing: false,
            game: None,
            gate: Gate::Closed,
            epoch: Epoch(0),
            holds: [Hold::Up; 256],
            established: false,
            reader: true,
        }
    }

    fn bound(bindings: &Bindings) -> Vec<(Chord, Target)> {
        bindings
            .iter()
            .filter_map(|(target, binding)| binding.0.map(|chord| (chord, target)))
            .collect()
    }

    /// The current epoch.
    #[must_use]
    pub const fn epoch(&self) -> Epoch {
        self.epoch
    }

    /// Whether keys can act.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        matches!(self.gate, Gate::Open)
    }

    /// Whether the console reader still authorises anything.
    #[must_use]
    pub const fn reads_console(&self) -> bool {
        self.reader
    }

    /// Sets the game whose window must be in front for a game-path hotkey. Pass the pinned process of the
    /// attached session, or `None` while there is none.
    pub const fn set_game(&mut self, pid: Option<u32>) {
        self.game = pid;
    }

    /// Closes the gate: nothing acts until [`Arbiter::reopen`] completes.
    pub const fn close(&mut self) {
        if !matches!(self.gate, Gate::Shut) {
            self.gate = Gate::Closed;
            self.epoch = Epoch(self.epoch.0 + 1);
        }
    }

    /// Closes the gate for good, as the trainer exits. No acknowledgement is needed.
    pub const fn shut(&mut self) {
        self.gate = Gate::Shut;
        self.epoch = Epoch(self.epoch.0 + 1);
    }

    /// Starts reopening the gate. Send the returned epoch through the platform's barrier; the gate opens when
    /// it comes back. `None` once shut.
    pub const fn reopen(&mut self) -> Option<Epoch> {
        if matches!(self.gate, Gate::Shut) {
            return None;
        }
        self.epoch = Epoch(self.epoch.0 + 1);
        self.gate = Gate::Reopening(self.epoch);
        Some(self.epoch)
    }

    /// Takes new bindings, or starts or ends a capture. The change is a barrier: if the gate was open or
    /// reopening, it closes, and the returned epoch must go through the platform as for
    /// [`Arbiter::reopen`]. Keys held across the change never act.
    pub fn rebind(&mut self, bindings: &Bindings, capturing: bool) -> Option<Epoch> {
        self.bound = Self::bound(bindings);
        self.capturing = capturing;
        match self.gate {
            Gate::Open | Gate::Reopening(_) => self.reopen(),
            Gate::Closed | Gate::Shut => None,
        }
    }

    /// The platform acknowledged `epoch`: every wake it sent before has been handled. If that is the epoch
    /// the gate waits for, flush the console and pass the result to [`Arbiter::open`].
    pub fn barrier(&self, epoch: Epoch) -> Option<Flush> {
        (self.gate == Gate::Reopening(epoch)).then_some(Flush(epoch))
    }

    /// Opens the gate after the console's waiting records were discarded. Every key held now is primed.
    pub fn open(&mut self, flush: Flush, probe: &impl Probe) {
        if self.gate == Gate::Reopening(flush.0) {
            self.prime(probe);
            // The flush left nothing older than now, so a console window in front is already established.
            self.established = probe.console() == ConsoleFocus::Front;
            self.gate = Gate::Open;
        }
    }

    /// The foreground window changed. Every held key is primed, and a classic console window entering the
    /// front authorises nothing until one batch has been read with it there. Read the console once after
    /// this, even if nothing is waiting.
    pub fn foreground(&mut self, probe: &impl Probe) {
        self.established = false;
        self.prime(probe);
    }

    /// A keyboard arrived or was removed: every held key is primed.
    pub fn devices(&mut self, probe: &impl Probe) {
        self.prime(probe);
    }

    /// The console can no longer be read: it authorises nothing for the rest of the session. The game path
    /// keeps working.
    pub const fn console_failed(&mut self) {
        self.reader = false;
    }

    /// Something happened on the keyboard: the game path. Releases are confirmed, and a fresh press of a bound
    /// key acts if the game's window is in front.
    pub fn physical(&mut self, probe: &impl Probe) -> Vec<Command> {
        let modifiers = probe.modifiers();
        let game_front = self.game.is_some() && probe.foreground() == self.game;
        let open = self.is_open() && !self.capturing;
        let mut commands = Vec::new();
        for vk in 0..=u8::MAX {
            let held = self.holds[usize::from(vk)] != Hold::Up;
            if !held && !self.watches(vk) {
                continue;
            }
            match (held, probe.down(vk)) {
                (true, false) => self.holds[usize::from(vk)] = Hold::Up,
                (false, true) => {
                    let target = (open && game_front)
                        .then(|| modifiers.and_then(|mods| self.target(mods, vk)))
                        .flatten();
                    // A press made with the game in front is decided now; elsewhere a console record may
                    // still claim it.
                    let spent = target.is_some() || game_front || !self.is_open();
                    self.holds[usize::from(vk)] = Hold::Down {
                        spent,
                        recorded: false,
                    };
                    commands.extend(target.map(Command::Hotkey));
                }
                _ => {}
            }
        }
        commands
    }

    /// A batch of console records: the console path.
    pub fn console(&mut self, batch: &Batch, probe: &impl Probe) -> Vec<Command> {
        if !self.reader {
            return Vec::new();
        }
        let focus = probe.console();
        let established = focus == ConsoleFocus::Front && self.established;
        // Records read while the game's window is in front were not typed into the console just now.
        let live = !(self.game.is_some() && probe.foreground() == self.game);
        let mut unbroken = true;
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut seen = [false; 256];
        let mut commands = Vec::new();
        for record in &batch.records {
            let key = match record {
                Record::Focus => {
                    candidates.clear();
                    unbroken = false;
                    self.prime(probe);
                    continue;
                }
                Record::Key(key) => key,
            };
            let Ok(vk) = u8::try_from(key.vk) else {
                continue;
            };
            // A modifier change cancels a chord still held; a completed tap survives it.
            candidates.retain(|c| c.released || c.modifiers == key.modifiers);
            if !key.down {
                for candidate in candidates.iter_mut().filter(|c| c.vk == vk) {
                    candidate.released = true;
                }
                if !probe.down(vk) {
                    self.holds[usize::from(vk)] = Hold::Up;
                }
                continue;
            }
            let (fresh, spent) = match self.holds[usize::from(vk)] {
                Hold::Up => (true, false),
                Hold::Down { spent, recorded } => (!recorded, spent),
            };
            self.holds[usize::from(vk)] = Hold::Down {
                spent,
                recorded: true,
            };
            seen[usize::from(vk)] = true;
            if !self.is_open() || !live {
                continue;
            }
            let target = if self.capturing {
                None
            } else {
                self.target(key.modifiers, vk)
            };
            match target {
                Some(target) => {
                    if fresh && !spent && unbroken {
                        candidates.push(Candidate {
                            vk,
                            modifiers: key.modifiers,
                            target,
                            released: false,
                        });
                    }
                }
                None => commands.extend(command(key, vk, fresh)),
            }
        }
        if batch.complete {
            let win_free = probe.modifiers().is_some();
            for candidate in candidates {
                let physical = !candidate.released && probe.down(candidate.vk);
                if win_free && (established || (focus != ConsoleFocus::Front && physical)) {
                    commands.push(Command::Hotkey(candidate.target));
                }
            }
            // Every press the console has seen is decided now; one already released needs no hold.
            for vk in (0..=u8::MAX).filter(|vk| seen[usize::from(*vk)]) {
                self.holds[usize::from(vk)] = if probe.down(vk) {
                    Hold::Down {
                        spent: true,
                        recorded: true,
                    }
                } else {
                    Hold::Up
                };
            }
            self.established = focus == ConsoleFocus::Front && unbroken;
        } else {
            self.established = false;
            self.prime(probe);
        }
        commands
    }

    fn watches(&self, vk: u8) -> bool {
        self.bound.iter().any(|(chord, _)| chord.key().vk() == vk)
    }

    fn target(&self, modifiers: Modifiers, vk: u8) -> Option<Target> {
        let key = Key::from_vk(vk)?;
        let chord = Chord::new(modifiers, key);
        self.bound
            .iter()
            .find(|(bound, _)| *bound == chord)
            .map(|(_, target)| *target)
    }

    /// Marks every key held now as spent and seen, and forgets every key that is up.
    fn prime(&mut self, probe: &impl Probe) {
        for vk in 0..=u8::MAX {
            self.holds[usize::from(vk)] = if probe.down(vk) {
                Hold::Down {
                    spent: true,
                    recorded: true,
                }
            } else {
                Hold::Up
            };
        }
    }
}

/// The dashboard command for a key that is not a hotkey: autorepeat only for scrolling keys, nothing for
/// modifiers or keys the dashboard has no name for.
fn command(key: &KeyRecord, vk: u8, fresh: bool) -> Option<Command> {
    if vk == VK_C && key.modifiers == Modifiers::CTRL {
        return fresh.then_some(Command::Interrupt);
    }
    let console_key = match vk {
        0x08 => ConsoleKey::Backspace,
        0x09 => ConsoleKey::Tab,
        0x0D => ConsoleKey::Enter,
        0x1B => ConsoleKey::Escape,
        0x25 => ConsoleKey::Left,
        0x26 => ConsoleKey::Up,
        0x27 => ConsoleKey::Right,
        0x28 => ConsoleKey::Down,
        _ => match Key::from_vk(vk) {
            Some(key) => ConsoleKey::Key(key),
            None => ConsoleKey::Char(key.text.filter(|c| !c.is_control())?),
        },
    };
    let repeats = matches!(
        console_key,
        ConsoleKey::Up | ConsoleKey::Down | ConsoleKey::Key(Key::PAGE_UP | Key::PAGE_DOWN)
    );
    (fresh || repeats).then_some(Command::Press(Press {
        key: console_key,
        modifiers: key.modifiers,
    }))
}

#[cfg(test)]
mod tests;
