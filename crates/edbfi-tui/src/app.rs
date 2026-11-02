// SPDX-License-Identifier: AGPL-3.0-only
//! The dashboard as a pure reducer: [`App::update`] takes one [`Msg`] and returns the [`Effect`]s the driver
//! must carry out, and drawing `&App` is a deterministic function of its state and the area.
//!
//! The app owns what the user wants: the settings and the master switch. The runtime owns the session; the
//! app only learns its [`Phase`] and each [`Ack`], and asks for publications. Rows show acknowledged state,
//! never the wish: a selected cheat reads ON only once a publication containing it is acknowledged.
//!
//! [`App::take_redraw`] reports whether anything visible changed, so an idle dashboard never draws.
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use edbfi_trainer::chord::{Binding, Chord, Key, Modifiers};
use edbfi_trainer::control::{Ack, ControlSet, DesiredMask, Master};
use edbfi_trainer::input::{Command, ConsoleKey, Press};
use edbfi_trainer::journal::Severity;
use edbfi_trainer::lifecycle::{Phase, RuntimeCommand, RuntimeEvent};
use edbfi_trainer::profile::{Desired, OptionId, Selection, ToggleId, ValidProfile};
use edbfi_trainer::settings::{External, Saved, Settings, StoreError, Target};

use crate::layout::{Layout, Window};
use crate::text;
use crate::theme::{Appearance, Tone};
use crate::view;

/// Where the trainer's files are, for display. `None` means there is no such file this session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Paths {
    /// The settings file; `None` when settings are not saved this session.
    pub settings: Option<PathBuf>,
    /// The log file; `None` when it could not be opened.
    pub log: Option<PathBuf>,
}

/// Everything the app starts from.
#[derive(Clone, Debug)]
pub struct Startup {
    /// The settings in use: as loaded, or defaults while [`Phase::Settings`] blocks.
    pub settings: Settings,
    /// The trainer's files.
    pub paths: Paths,
    /// The runtime's phase.
    pub phase: Phase,
    /// The colour depth and glyphs.
    pub appearance: Appearance,
    /// The terminal size in cells, width then height.
    pub size: (u16, u16),
}

/// Something that happened.
#[derive(Debug)]
pub enum Msg {
    /// The runtime reported something.
    Runtime(RuntimeEvent),
    /// The user did something.
    Input(Command),
    /// The terminal changed size.
    Resize {
        /// Columns.
        width: u16,
        /// Rows.
        height: u16,
    },
    /// The driver finished an [`Effect::Save`].
    Saved(Result<Saved, StoreError>),
    /// The runtime resolved a [`Phase::Settings`] problem; these settings now apply.
    Loaded {
        /// The settings.
        settings: Settings,
        /// The settings file, or `None` if these settings are never saved.
        path: Option<PathBuf>,
    },
    /// The console can no longer be read or drawn; the driver switched to plain output.
    ConsoleLost,
}

/// Something the driver must do. Within one batch, carry out runtime commands, then saves, then draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Effect {
    /// Send this command to the runtime.
    Runtime(RuntimeCommand),
    /// Save [`App::settings`], then report the result with [`Msg::Saved`].
    Save,
    /// The hotkeys or the capture state changed: re-read [`App::settings`] and [`App::capturing`].
    Bindings,
    /// Play this sound.
    Chime(Chime),
    /// Set the window title to [`App::title`].
    Title,
}

/// The sound played when the cheats the game sees change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Chime {
    /// Something turned on.
    Rise,
    /// Only things turned off.
    Fall,
}

/// A focusable row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Row {
    Master,
    Toggle(ToggleId),
    Option(OptionId),
    Sounds,
    StartPaused,
}

/// A list the user moves through. Wide screens have two panes; compact screens one tab per group plus options.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Pane {
    Left,
    Right,
    Tab(usize),
}

/// One line of a pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Item {
    Heading(Heading),
    Blank,
    Row(Row),
}

/// A section heading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Heading {
    Trainer,
    Group(usize),
    Options,
}

/// What a row shows in its status column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Status {
    On,
    Via,
    Paused,
    Armed,
    Off,
    Active,
    Inactive,
}

impl Status {
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::On => "ON",
            Self::Via => "VIA",
            Self::Paused => "PAUSED",
            Self::Armed => "ARMED",
            Self::Off => "OFF",
            Self::Active => "ACTIVE",
            Self::Inactive => "INACTIVE",
        }
    }

    pub(crate) const fn tone(self) -> Tone {
        match self {
            Self::On | Self::Active => Tone::On,
            Self::Via => Tone::Via,
            Self::Paused => Tone::Paused,
            Self::Armed => Tone::Armed,
            Self::Off | Self::Inactive => Tone::Muted,
        }
    }
}

/// A dialog over the dashboard. Escape closes it first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Modal {
    Help {
        scroll: usize,
    },
    Capture {
        target: Target,
        /// The chord or removal pressed so far, applied on Enter.
        candidate: Option<Binding>,
        refusal: Option<String>,
    },
    Conflict {
        target: Target,
        chord: Chord,
        holder: Target,
    },
    Quit {
        quit: bool,
        interrupted: bool,
    },
}

/// What the status line reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Last {
    /// The user changed this row; the line shows its current status.
    Row(Row),
    /// The user bound this target.
    Bound(Target),
    /// A message.
    Message { text: String, tone: Tone },
}

/// Whether the latest change is on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SaveState {
    Idle,
    Pending,
    Saved,
    Failed,
    SessionOnly,
}

/// A notice that stays until something resolves it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notice {
    pub(crate) text: String,
    pub(crate) tone: Tone,
    /// Cleared by the next save that succeeds; otherwise by the next console action.
    pub(crate) until_saved: bool,
}

/// Everything visible. Any change marks the frame for redrawing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Model {
    pub(crate) settings: Settings,
    pub(crate) paths: Paths,
    pub(crate) phase: Phase,
    pub(crate) master: Master,
    pub(crate) acked: Option<ControlSet>,
    /// The selection that produced the acknowledged mask.
    pub(crate) acked_selection: Option<Selection>,
    pub(crate) focus: Row,
    pub(crate) marks: BTreeMap<Pane, Row>,
    pub(crate) scroll: BTreeMap<Pane, usize>,
    pub(crate) modal: Option<Modal>,
    pub(crate) card_scroll: usize,
    pub(crate) last: Option<Last>,
    pub(crate) notice: Option<Notice>,
    /// Where the latest outside edit of the settings file was kept; Help shows it for the session.
    pub(crate) kept: Option<PathBuf>,
    /// Why the latest save failed; Help shows it until a save succeeds.
    pub(crate) save_error: Option<String>,
    /// Set while the settings problem is unresolved, so the defaults shown meanwhile are never saved over the
    /// user's file; [`Msg::Loaded`] clears it.
    pub(crate) awaiting_load: bool,
    pub(crate) save: SaveState,
    pub(crate) size: (u16, u16),
    pub(crate) appearance: Appearance,
    pub(crate) plain: bool,
    pub(crate) quitting: bool,
}

/// A profile group and its toggles.
#[derive(Clone, Debug)]
pub(crate) struct GroupRows {
    pub(crate) title: &'static str,
    pub(crate) toggles: Vec<ToggleId>,
    /// Whether every toggle selects a weapon class; wide screens show such groups on the right.
    pub(crate) classes: bool,
}

/// The dashboard.
#[derive(Debug)]
pub struct App<'p> {
    pub(crate) profile: ValidProfile<'p>,
    pub(crate) groups: Vec<GroupRows>,
    pub(crate) model: Model,
    published: Option<DesiredMask>,
    /// Publications not yet acknowledged, oldest first, with the selection each came from.
    in_flight: VecDeque<(DesiredMask, Selection)>,
    revision: Option<u64>,
    title: String,
    dirty: bool,
    effects: Vec<Effect>,
}

const fn letter(byte: u8) -> Key {
    match Key::letter(byte) {
        Some(key) => key,
        None => panic!("not a letter"),
    }
}

const KEY_D: Key = letter(b'D');
const KEY_H: Key = letter(b'H');
const KEY_P: Key = letter(b'P');
const KEY_Q: Key = letter(b'Q');
const KEY_R: Key = letter(b'R');
const KEY_S: Key = letter(b'S');

/// Whether `press` is `key` alone, or with Shift for letters typed with Caps Lock.
fn is_key(press: Press, key: Key) -> bool {
    press.key == ConsoleKey::Key(key) && Modifiers::SHIFT.contains(press.modifiers)
}

fn is_plain(press: Press, key: ConsoleKey) -> bool {
    press.key == key && press.modifiers.is_empty()
}

fn is_char(press: Press, c: char) -> bool {
    press.key == ConsoleKey::Char(c) && Modifiers::SHIFT.contains(press.modifiers)
}

impl<'p> App<'p> {
    /// A dashboard for `profile`, starting from `startup`. Set the window title from [`App::title`], then carry
    /// out [`App::take_effects`].
    ///
    /// # Panics
    ///
    /// If the settings belong to another profile.
    #[must_use]
    pub fn new(profile: ValidProfile<'p>, startup: Startup) -> Self {
        let class_toggles: Vec<ToggleId> = profile
            .classes()
            .map(|(id, _)| profile.class_toggle(id))
            .collect();
        let groups = profile
            .profile()
            .groups
            .iter()
            .map(|group| {
                let toggles: Vec<ToggleId> = profile
                    .toggles()
                    .filter(|(_, toggle)| toggle.group == group.key)
                    .map(|(id, _)| id)
                    .collect();
                let classes =
                    !toggles.is_empty() && toggles.iter().all(|id| class_toggles.contains(id));
                GroupRows {
                    title: group.title,
                    toggles,
                    classes,
                }
            })
            .collect();
        let save = if startup.paths.settings.is_some() {
            SaveState::Idle
        } else {
            SaveState::SessionOnly
        };
        let focus = profile
            .toggles()
            .next()
            .map_or(Row::Master, |(id, _)| Row::Toggle(id));
        let mut app = Self {
            profile,
            groups,
            model: Model {
                settings: startup.settings,
                paths: startup.paths,
                phase: Phase::Waiting,
                master: Master::Active,
                acked: None,
                acked_selection: None,
                focus,
                marks: BTreeMap::new(),
                scroll: BTreeMap::new(),
                modal: None,
                card_scroll: 0,
                last: None,
                notice: None,
                kept: None,
                save_error: None,
                awaiting_load: false,
                save,
                size: startup.size,
                appearance: startup.appearance,
                plain: false,
                quitting: false,
            },
            published: None,
            in_flight: VecDeque::new(),
            revision: None,
            title: String::new(),
            dirty: true,
            effects: Vec::new(),
        };
        // The settings must index this profile; checking one value panics early if they do not.
        let _ = app.model.settings.selection.contains(app.first_toggle());
        app.enter(startup.phase);
        app.follow();
        app.title = app.compute_title();
        app
    }

    /// Applies `msg` and returns what the driver must do, in order.
    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        let before = self.model.clone();
        match msg {
            Msg::Runtime(RuntimeEvent::Phase(phase)) => self.enter(phase),
            Msg::Runtime(RuntimeEvent::Ack(ack)) => self.acknowledge(ack),
            Msg::Runtime(RuntimeEvent::Journal(entry)) => {
                let failed_save = self.model.notice.as_ref().is_some_and(|n| n.until_saved);
                if entry.event.severity >= Severity::Warning && !failed_save {
                    let tone = if entry.event.severity == Severity::Error {
                        Tone::Error
                    } else {
                        Tone::Armed
                    };
                    self.model.notice = Some(Notice {
                        text: entry.event.message,
                        tone,
                        until_saved: false,
                    });
                }
            }
            Msg::Input(command) => self.input(command),
            Msg::Resize { width, height } => self.model.size = (width, height),
            Msg::Saved(result) => self.saved(result),
            Msg::Loaded { settings, path } => {
                let _ = settings.selection.contains(self.first_toggle());
                self.model.save = if path.is_some() {
                    SaveState::Idle
                } else {
                    SaveState::SessionOnly
                };
                self.model.settings = settings;
                self.model.paths.settings = path;
                self.model.awaiting_load = false;
                self.model.save_error = None;
                if self.model.notice.as_ref().is_some_and(|n| n.until_saved) {
                    self.model.notice = None;
                }
                self.effects.push(Effect::Bindings);
                self.publish();
            }
            Msg::ConsoleLost => {
                if matches!(
                    self.model.modal,
                    Some(Modal::Capture { .. } | Modal::Conflict { .. })
                ) {
                    self.effects.push(Effect::Bindings);
                }
                self.model.modal = None;
                self.model.plain = true;
            }
        }
        self.follow();
        if self.model != before {
            self.dirty = true;
        }
        let title = self.compute_title();
        if title != self.title {
            self.title = title;
            self.effects.push(Effect::Title);
        }
        std::mem::take(&mut self.effects)
    }

    /// Effects queued outside [`App::update`]: those of the starting phase. Call once after [`App::new`].
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// Whether anything visible changed since the last call; the driver draws only then.
    pub fn take_redraw(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// The settings to save and the hotkeys to listen for.
    #[must_use]
    pub const fn settings(&self) -> &Settings {
        &self.model.settings
    }

    /// The window title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Whether the next chord pressed in the console sets a hotkey; hotkeys must not fire meanwhile.
    #[must_use]
    pub const fn capturing(&self) -> bool {
        matches!(
            self.model.modal,
            Some(Modal::Capture { .. } | Modal::Conflict { .. })
        )
    }

    /// Whether the console was lost and output is plain.
    #[must_use]
    pub const fn plain(&self) -> bool {
        self.model.plain
    }

    /// The runtime's phase as last reported.
    #[must_use]
    pub const fn phase(&self) -> &Phase {
        &self.model.phase
    }

    /// What the app wants published, while attached.
    #[must_use]
    pub fn desired(&self) -> Desired {
        Desired {
            master: self.model.master,
            selection: self.model.settings.selection,
            options: self.model.settings.options,
        }
    }

    fn first_toggle(&self) -> ToggleId {
        // Validation requires at least one toggle.
        self.profile
            .toggles()
            .next()
            .map(|(id, _)| id)
            .unwrap_or_else(|| unreachable!("a valid profile has toggles"))
    }

    // ----- runtime -----

    fn enter(&mut self, phase: Phase) {
        let was_attached = self.model.phase.is_attached();
        let card = matches!(
            phase,
            Phase::Settings(_) | Phase::Failed(_) | Phase::Finished(_)
        );
        if phase != self.model.phase {
            self.model.card_scroll = 0;
        }
        self.model.phase = phase;
        if matches!(self.model.phase, Phase::Settings(_)) {
            self.model.awaiting_load = true;
        }
        // A failure while quitting, such as a failed restore, must stay readable and answerable.
        if matches!(self.model.phase, Phase::Failed(_)) {
            self.model.quitting = false;
        }
        if card || (!self.model.phase.is_attached() && self.quit_open()) {
            if self.capturing() {
                self.effects.push(Effect::Bindings);
            }
            self.model.modal = None;
        }
        if self.model.phase.is_attached() && !was_attached {
            self.model.master = self.model.settings.master_on_connect();
            self.model.acked = None;
            self.published = None;
            self.in_flight.clear();
            self.model.acked_selection = None;
            self.revision = None;
            self.publish();
        } else if !self.model.phase.is_attached() {
            self.model.acked = None;
            self.published = None;
            self.in_flight.clear();
            self.model.acked_selection = None;
            self.revision = None;
        }
    }

    fn quit_open(&self) -> bool {
        matches!(self.model.modal, Some(Modal::Quit { .. }))
    }

    fn acknowledge(&mut self, ack: Ack) {
        if !self.model.phase.is_attached() || self.revision.is_some_and(|r| ack.revision() <= r) {
            return;
        }
        self.revision = Some(ack.revision());
        let old = self.model.acked.unwrap_or(ControlSet::EMPTY);
        let new = ack.mask().bits();
        self.model.acked = Some(new);
        // Acknowledgements arrive in publication order, so the first matching publication is this one.
        if let Some(at) = self
            .in_flight
            .iter()
            .position(|(mask, _)| mask.bits() == new)
        {
            self.in_flight.drain(..at);
            self.model.acked_selection = self.in_flight.pop_front().map(|(_, selection)| selection);
        }
        if new != old && self.model.settings.sounds {
            let rose = new.word() & !old.word() != 0;
            self.effects
                .push(Effect::Chime(if rose { Chime::Rise } else { Chime::Fall }));
        }
    }

    fn publish(&mut self) {
        // After Quit nothing but the runtime's own zero may reach the game.
        if !self.model.phase.is_attached() || self.model.quitting {
            return;
        }
        let mask = self.profile.effective(&self.desired());
        let selection = self.model.settings.selection;
        if self.published != Some(mask) {
            self.published = Some(mask);
            self.in_flight.push_back((mask, selection));
            self.effects
                .push(Effect::Runtime(RuntimeCommand::Publish(mask)));
        } else if let Some((_, last)) = self.in_flight.back_mut() {
            // Same mask, new selection: the publication in flight now stands for this selection.
            *last = selection;
        } else if self.model.acked == Some(mask.bits()) {
            // A selection producing the acknowledged mask is itself acknowledged.
            self.model.acked_selection = Some(selection);
        }
    }

    fn saved(&mut self, result: Result<Saved, StoreError>) {
        if self.model.paths.settings.is_none() {
            return;
        }
        match result {
            Ok(saved) => {
                self.model.save = SaveState::Saved;
                self.model.save_error = None;
                if self.model.notice.as_ref().is_some_and(|n| n.until_saved) {
                    self.model.notice = None;
                }
                let external = match saved {
                    Saved::Written { external } => external,
                    Saved::Unchanged => None,
                };
                match external {
                    Some(External::Kept(path)) => {
                        self.model.kept = Some(path.clone());
                        // Split on both separators, so a Windows path names its file on any host.
                        let shown = path.display().to_string();
                        let name = shown.rsplit(['\\', '/']).next().unwrap_or(&shown);
                        self.model.notice = Some(Notice {
                            text: format!("Outside edit kept as {name}; ? shows where"),
                            tone: Tone::Armed,
                            until_saved: false,
                        });
                    }
                    Some(External::Deleted) => {
                        self.model.notice = Some(Notice {
                            text: "The settings file had been deleted; it was written again".into(),
                            tone: Tone::Armed,
                            until_saved: false,
                        });
                    }
                    None => {}
                }
            }
            Err(error) => {
                self.model.save = SaveState::Failed;
                // The cause says what to fix; the path is in the help and the log.
                let reason = std::error::Error::source(&error)
                    .map_or_else(|| error.to_string(), ToString::to_string);
                self.model.save_error = Some(reason);
                self.model.notice = Some(Notice {
                    text: "Save failed; S retries, ? shows why".into(),
                    tone: Tone::Error,
                    until_saved: true,
                });
            }
        }
    }

    // ----- input -----

    /// Whether keys wait: the runtime is writing or restoring code, it has finished, or the trainer is quitting.
    pub(crate) fn input_closed(&self) -> bool {
        self.model.quitting
            || matches!(
                self.model.phase,
                Phase::Installing | Phase::Restoring | Phase::Finished(_)
            )
    }

    fn input(&mut self, command: Command) {
        if self.model.quitting {
            return;
        }
        match command {
            Command::Interrupt => self.interrupt(),
            Command::Hotkey(target) => {
                if self.input_closed()
                    || matches!(self.model.phase, Phase::Settings(_))
                    || self.capturing()
                {
                    return;
                }
                match target {
                    Target::Master => self.toggle_master(),
                    Target::Toggle(id) => self.toggle(id),
                }
            }
            Command::Press(press) => {
                if self.model.plain || self.input_closed() {
                    return;
                }
                if self.model.notice.as_ref().is_some_and(|n| !n.until_saved) {
                    self.model.notice = None;
                }
                if let Some(modal) = self.model.modal.clone() {
                    self.modal_press(modal, press);
                    return;
                }
                match self.model.phase.clone() {
                    Phase::Settings(problem) => self.settings_press(&problem, press),
                    Phase::Failed(failure) => {
                        if self.card_scroll_press(press) {
                            return;
                        }
                        if is_plain(press, ConsoleKey::Enter) && failure.can_retry() {
                            self.effects.push(Effect::Runtime(RuntimeCommand::Retry));
                        } else if is_key(press, KEY_Q) || is_plain(press, ConsoleKey::Escape) {
                            self.quit();
                        }
                    }
                    _ => self.list_press(press),
                }
            }
        }
    }

    fn interrupt(&mut self) {
        match self.model.phase {
            Phase::Restoring | Phase::Finished(_) => {}
            // A plain console cannot show a dialog, so the first interrupt quits.
            _ if self.quit_open() || self.model.plain => self.quit(),
            Phase::Attached { .. } | Phase::Installing => {
                if self.capturing() {
                    self.effects.push(Effect::Bindings);
                }
                self.model.modal = Some(Modal::Quit {
                    quit: false,
                    interrupted: true,
                });
            }
            _ => self.quit(),
        }
    }

    fn quit(&mut self) {
        if self.capturing() {
            self.effects.push(Effect::Bindings);
        }
        self.model.quitting = true;
        self.model.modal = None;
        self.effects.push(Effect::Runtime(RuntimeCommand::Quit));
    }

    fn quit_request(&mut self) {
        if self.model.phase.is_attached() {
            self.model.modal = Some(Modal::Quit {
                quit: false,
                interrupted: false,
            });
        } else {
            self.quit();
        }
    }

    fn card_scroll_press(&mut self, press: Press) -> bool {
        if !press.modifiers.is_empty() {
            return false;
        }
        let step = match press.key {
            ConsoleKey::Up => -1,
            ConsoleKey::Down => 1,
            ConsoleKey::Key(Key::PAGE_UP) => -5,
            ConsoleKey::Key(Key::PAGE_DOWN) => 5,
            _ => return false,
        };
        let max = view::card_scroll_max(self);
        self.model.card_scroll = self.model.card_scroll.saturating_add_signed(step).min(max);
        true
    }

    fn settings_press(
        &mut self,
        problem: &edbfi_trainer::lifecycle::SettingsProblem,
        press: Press,
    ) {
        use edbfi_trainer::lifecycle::SettingsProblem;
        if self.card_scroll_press(press) {
            return;
        }
        let command = if is_plain(press, ConsoleKey::Enter) {
            Some(RuntimeCommand::Retry)
        } else if is_key(press, KEY_D) && matches!(problem, SettingsProblem::Invalid { .. }) {
            Some(RuntimeCommand::ResetSettings)
        } else if is_key(press, KEY_S)
            && matches!(
                problem,
                SettingsProblem::Store {
                    session_only: true,
                    ..
                }
            )
        {
            Some(RuntimeCommand::SessionOnly)
        } else {
            None
        };
        if let Some(command) = command {
            self.effects.push(Effect::Runtime(command));
        } else if is_key(press, KEY_Q) || is_plain(press, ConsoleKey::Escape) {
            self.quit();
        }
    }

    fn list_press(&mut self, press: Press) {
        let shift = press.modifiers == Modifiers::SHIFT;
        match press.key {
            ConsoleKey::Up if press.modifiers.is_empty() => self.step(-1),
            ConsoleKey::Down if press.modifiers.is_empty() => self.step(1),
            ConsoleKey::Key(Key::HOME) if press.modifiers.is_empty() => self.step(isize::MIN),
            ConsoleKey::Key(Key::END) if press.modifiers.is_empty() => self.step(isize::MAX),
            ConsoleKey::Key(Key::PAGE_UP) if press.modifiers.is_empty() => {
                self.step(-self.page());
            }
            ConsoleKey::Key(Key::PAGE_DOWN) if press.modifiers.is_empty() => {
                self.step(self.page());
            }
            ConsoleKey::Tab if press.modifiers.is_empty() || shift => {
                self.switch(if shift { -1 } else { 1 });
            }
            ConsoleKey::Left if press.modifiers.is_empty() => self.switch(-1),
            ConsoleKey::Right if press.modifiers.is_empty() => self.switch(1),
            ConsoleKey::Enter if press.modifiers.is_empty() => self.activate(),
            ConsoleKey::Key(Key::SPACE) if press.modifiers.is_empty() => self.activate(),
            ConsoleKey::Escape if press.modifiers.is_empty() => self.quit_request(),
            _ if is_key(press, KEY_Q) => self.quit_request(),
            _ if is_key(press, KEY_R) => self.capture(),
            _ if is_key(press, KEY_P) => self.toggle_master(),
            _ if is_key(press, KEY_S) && self.model.save == SaveState::Failed => {
                self.request_save()
            }
            _ if is_key(press, KEY_H) || is_char(press, '?') => {
                self.model.modal = Some(Modal::Help { scroll: 0 });
            }
            _ => {}
        }
    }

    fn modal_press(&mut self, modal: Modal, press: Press) {
        match modal {
            Modal::Help { scroll } => {
                let step = match press.key {
                    ConsoleKey::Up if press.modifiers.is_empty() => -1,
                    ConsoleKey::Down if press.modifiers.is_empty() => 1,
                    ConsoleKey::Key(Key::PAGE_UP) if press.modifiers.is_empty() => -5,
                    ConsoleKey::Key(Key::PAGE_DOWN) if press.modifiers.is_empty() => 5,
                    ConsoleKey::Key(Key::HOME) if press.modifiers.is_empty() => isize::MIN,
                    ConsoleKey::Key(Key::END) if press.modifiers.is_empty() => isize::MAX,
                    _ => {
                        if is_plain(press, ConsoleKey::Escape)
                            || is_plain(press, ConsoleKey::Enter)
                            || is_key(press, KEY_Q)
                            || is_key(press, KEY_H)
                            || is_char(press, '?')
                        {
                            self.model.modal = None;
                        }
                        return;
                    }
                };
                let max = view::help_scroll_max(self);
                let scroll = scroll.saturating_add_signed(step).min(max);
                self.model.modal = Some(Modal::Help { scroll });
            }
            Modal::Capture {
                target, candidate, ..
            } => self.capture_press(target, candidate, press),
            Modal::Conflict {
                target,
                chord,
                holder,
            } => {
                if is_plain(press, ConsoleKey::Enter) {
                    self.swap(target, chord, holder);
                } else if is_plain(press, ConsoleKey::Escape) {
                    self.model.modal = None;
                    self.effects.push(Effect::Bindings);
                }
            }
            // Alt+Enter toggles full screen in a Windows console; only unmodified keys answer.
            Modal::Quit { .. } if !press.modifiers.is_empty() => {}
            Modal::Quit { quit, interrupted } => match press.key {
                ConsoleKey::Left | ConsoleKey::Right | ConsoleKey::Tab => {
                    self.model.modal = Some(Modal::Quit {
                        quit: !quit,
                        interrupted,
                    });
                }
                ConsoleKey::Enter if quit => self.quit(),
                ConsoleKey::Enter | ConsoleKey::Escape => self.model.modal = None,
                _ => {}
            },
        }
    }

    // ----- lists -----

    /// The panes of the current layout, each with its lines.
    pub(crate) fn panes(&self) -> Vec<(Pane, Vec<Item>)> {
        let (width, height) = self.model.size;
        let options = || {
            self.profile
                .options()
                .map(|(id, _)| Item::Row(Row::Option(id)))
                .chain([Item::Row(Row::Sounds), Item::Row(Row::StartPaused)])
        };
        let rows = |group: &GroupRows| {
            group
                .toggles
                .iter()
                .map(|id| Item::Row(Row::Toggle(*id)))
                .collect::<Vec<_>>()
        };
        if Layout::of(width, height) == Layout::Wide {
            let mut left = vec![
                Item::Heading(Heading::Trainer),
                Item::Row(Row::Master),
                Item::Blank,
            ];
            let mut right = Vec::new();
            for (index, group) in self.groups.iter().enumerate() {
                if group.classes {
                    // The wide view pins the first class group's title above the pane; later ones get headings.
                    if !right.is_empty() {
                        right.push(Item::Blank);
                        right.push(Item::Heading(Heading::Group(index)));
                    }
                    right.extend(rows(group));
                } else {
                    left.push(Item::Heading(Heading::Group(index)));
                    left.extend(rows(group));
                    left.push(Item::Blank);
                }
            }
            left.push(Item::Heading(Heading::Options));
            left.extend(options());
            let mut panes = vec![(Pane::Left, left)];
            if !right.is_empty() {
                panes.push((Pane::Right, right));
            }
            panes
        } else {
            let mut panes: Vec<_> = self
                .groups
                .iter()
                .enumerate()
                .map(|(index, group)| (Pane::Tab(index), rows(group)))
                .collect();
            let tab = Pane::Tab(self.groups.len());
            panes.push((
                tab,
                std::iter::once(Item::Row(Row::Master))
                    .chain(options())
                    .collect(),
            ));
            panes
        }
    }

    /// The pane holding the focus, its lines and the focus's line.
    pub(crate) fn focused_pane(&self) -> (Pane, Vec<Item>, usize) {
        let panes = self.panes();
        for (pane, items) in &panes {
            if let Some(line) = items
                .iter()
                .position(|item| *item == Item::Row(self.model.focus))
            {
                return (*pane, items.clone(), line);
            }
        }
        unreachable!("every row is in a pane")
    }

    fn rows_of(items: &[Item]) -> Vec<Row> {
        items
            .iter()
            .filter_map(|item| match item {
                Item::Row(row) => Some(*row),
                _ => None,
            })
            .collect()
    }

    fn page(&self) -> isize {
        let (pane, _, _) = self.focused_pane();
        let height = view::pane_height(self, pane);
        isize::try_from(height.saturating_sub(1).max(1)).unwrap_or(1)
    }

    fn step(&mut self, by: isize) {
        let (_, items, _) = self.focused_pane();
        let rows = Self::rows_of(&items);
        let Some(at) = rows.iter().position(|row| *row == self.model.focus) else {
            return;
        };
        let to = at
            .saturating_add_signed(by)
            .min(rows.len().saturating_sub(1));
        self.model.focus = rows[to];
    }

    fn switch(&mut self, by: isize) {
        let panes = self.panes();
        let (pane, _, _) = self.focused_pane();
        let Some(at) = panes.iter().position(|(p, _)| *p == pane) else {
            return;
        };
        let count = panes.len();
        let to = (at + count).saturating_add_signed(by) % count;
        if to == at {
            return;
        }
        self.model.marks.insert(pane, self.model.focus);
        let (next, items) = &panes[to];
        let rows = Self::rows_of(items);
        self.model.focus = self
            .model
            .marks
            .get(next)
            .copied()
            .filter(|row| rows.contains(row))
            .or_else(|| rows.first().copied())
            .unwrap_or(self.model.focus);
    }

    /// Keeps every pane's scroll position valid and the focus visible.
    fn follow(&mut self) {
        let (focused, _, _) = self.focused_pane();
        for (pane, items) in self.panes() {
            let height = view::pane_height(self, pane);
            let top = self.model.scroll.get(&pane).copied().unwrap_or(0);
            let focus = (pane == focused)
                .then(|| {
                    items
                        .iter()
                        .position(|item| *item == Item::Row(self.model.focus))
                })
                .flatten();
            // Moving up onto a section's first row also shows its heading.
            let top = match focus {
                Some(line) if line < top => line.saturating_sub(usize::from(matches!(
                    line.checked_sub(1).map(|l| items[l]),
                    Some(Item::Heading(_))
                ))),
                _ => top,
            };
            let window = Window::follow(items.len(), height, top, focus);
            self.model.scroll.insert(pane, window.top);
        }
    }

    // ----- actions -----

    fn activate(&mut self) {
        let row = self.model.focus;
        match row {
            Row::Master => self.toggle_master(),
            Row::Toggle(id) => self.toggle(id),
            Row::Option(id) => {
                let on = self.model.settings.options.contains(id);
                self.model.settings.options.set(id, !on);
                self.changed(row);
                self.publish();
            }
            Row::Sounds => {
                self.model.settings.sounds = !self.model.settings.sounds;
                self.changed(row);
            }
            Row::StartPaused => {
                self.model.settings.start_paused = !self.model.settings.start_paused;
                self.changed(row);
            }
        }
    }

    fn toggle(&mut self, id: ToggleId) {
        let on = self.model.settings.selection.contains(id);
        self.model.settings.selection.set(id, !on);
        self.changed(Row::Toggle(id));
        self.publish();
    }

    fn toggle_master(&mut self) {
        if !self.model.phase.is_attached() {
            self.model.last = Some(Last::Message {
                text: "Pause works once the game is connected".into(),
                tone: Tone::Muted,
            });
            return;
        }
        self.model.master = self.model.master.toggled();
        self.model.last = Some(Last::Row(Row::Master));
        self.publish();
    }

    /// Records a settings change to `row` and asks for a save.
    fn changed(&mut self, row: Row) {
        self.model.last = Some(Last::Row(row));
        self.request_save();
    }

    fn request_save(&mut self) {
        if self.model.awaiting_load {
            // Never save the defaults shown behind a settings problem; say so instead.
            self.model.save = SaveState::SessionOnly;
        } else if self.model.paths.settings.is_some() {
            self.model.save = SaveState::Pending;
            self.effects.push(Effect::Save);
        }
    }

    fn capture(&mut self) {
        let target = match self.model.focus {
            Row::Master => Target::Master,
            Row::Toggle(id) => Target::Toggle(id),
            _ => {
                self.model.last = Some(Last::Message {
                    text: "Only cheats and the master switch take hotkeys".into(),
                    tone: Tone::Muted,
                });
                return;
            }
        };
        self.model.modal = Some(Modal::Capture {
            target,
            candidate: None,
            refusal: None,
        });
        self.effects.push(Effect::Bindings);
    }

    /// A pressed chord is only previewed; Enter applies it, Escape discards it.
    fn capture_press(&mut self, target: Target, candidate: Option<Binding>, press: Press) {
        let preview = |candidate, refusal| Modal::Capture {
            target,
            candidate,
            refusal,
        };
        if is_plain(press, ConsoleKey::Escape) {
            self.model.modal = None;
            self.effects.push(Effect::Bindings);
        } else if is_plain(press, ConsoleKey::Enter) {
            match candidate {
                Some(Binding(Some(chord)))
                    if self
                        .model
                        .settings
                        .bindings
                        .holder(chord)
                        .is_some_and(|holder| holder != target) =>
                {
                    let holder = self.model.settings.bindings.holder(chord).unwrap_or(target);
                    self.model.modal = Some(Modal::Conflict {
                        target,
                        chord,
                        holder,
                    });
                }
                Some(binding) => self.bind(target, binding),
                None => {}
            }
        } else if is_plain(press, ConsoleKey::Backspace) {
            self.model.modal = Some(preview(Some(Binding(None)), None));
        } else if let Some(chord) = press.chord() {
            self.model.modal = Some(match chord.check_policy() {
                Ok(()) => preview(Some(Binding(Some(chord))), None),
                Err(refusal) => preview(None, Some(format!("{chord}: {refusal}"))),
            });
        } else {
            self.model.modal = Some(preview(None, Some("That key can't be a hotkey.".into())));
        }
    }

    fn bind(&mut self, target: Target, binding: Binding) {
        self.model.modal = None;
        self.effects.push(Effect::Bindings);
        let current = self.model.settings.bindings.get(target);
        if current == binding {
            self.model.last = Some(Last::Bound(target));
            return;
        }
        match self.model.settings.bindings.assign(target, binding) {
            Ok(_) => {
                self.model.last = Some(Last::Bound(target));
                self.request_save();
            }
            Err(error) => {
                self.model.last = Some(Last::Message {
                    text: format!("Hotkey not changed: {error}"),
                    tone: Tone::Error,
                });
            }
        }
    }

    fn swap(&mut self, target: Target, chord: Chord, holder: Target) {
        self.model.modal = None;
        self.effects.push(Effect::Bindings);
        let bindings = &mut self.model.settings.bindings;
        let old = bindings.get(target);
        // Free the chord, give the holder the target's old chord, then take the chord. Each step keeps every
        // chord unique, and both chords already passed the policy.
        let result = bindings
            .assign(holder, Binding(None))
            .and_then(|_| bindings.assign(target, Binding(Some(chord))))
            .and_then(|_| bindings.assign(holder, old));
        match result {
            Ok(_) => {
                self.model.last = Some(Last::Bound(target));
                self.request_save();
            }
            Err(error) => {
                self.model.last = Some(Last::Message {
                    text: format!("Hotkeys not swapped: {error}"),
                    tone: Tone::Error,
                });
            }
        }
    }

    // ----- what rows show -----

    /// A selected toggle other than `id` whose bits include all of `id`'s, if any.
    pub(crate) fn coverer(&self, id: ToggleId) -> Option<ToggleId> {
        let bits = self.profile.toggle(id).controls.word();
        if bits == 0 {
            return None;
        }
        self.profile
            .toggles()
            .find(|(other, toggle)| {
                *other != id
                    && self.model.settings.selection.contains(*other)
                    && toggle.controls.word() & bits == bits
            })
            .map(|(other, _)| other)
    }

    /// A toggle other than `id` whose bits include all of `id`'s and are all acknowledged, if any: what the
    /// game is applying `id` through, whatever the selection now says.
    pub(crate) fn acked_coverer(&self, id: ToggleId) -> Option<ToggleId> {
        let bits = self.profile.toggle(id).controls.word();
        let acked = self.model.acked.unwrap_or(ControlSet::EMPTY).word();
        if bits == 0 || acked & bits != bits {
            return None;
        }
        self.profile
            .toggles()
            .find(|(other, toggle)| {
                let own = toggle.controls.word();
                *other != id
                    && own & bits == bits
                    && acked & own == own
                    && self
                        .model
                        .acked_selection
                        .is_none_or(|selection| selection.contains(*other))
            })
            .map(|(other, _)| other)
    }

    /// What `row` shows in its status column.
    pub(crate) fn status(&self, row: Row) -> Status {
        let attached = self.model.phase.is_attached();
        let on = |b: bool| if b { Status::On } else { Status::Off };
        match row {
            Row::Master if attached => match self.model.master {
                Master::Active => Status::Active,
                Master::Paused => Status::Paused,
            },
            Row::Master => Status::Inactive,
            Row::Option(id) => on(self.model.settings.options.contains(id)),
            Row::Sounds => on(self.model.settings.sounds),
            Row::StartPaused => on(self.model.settings.start_paused),
            Row::Toggle(id) => {
                let selected = self.model.settings.selection.contains(id);
                if !attached {
                    return if selected { Status::Armed } else { Status::Off };
                }
                let bits = self.profile.toggle(id).controls.word();
                let acked = self.model.acked.unwrap_or(ControlSet::EMPTY).word();
                let covered = self.coverer(id).is_some();
                if bits != 0 && acked & bits == bits {
                    // Applied: through this row's own acknowledged selection, or through another one's.
                    let own = self
                        .model
                        .acked_selection
                        .is_some_and(|selection| selection.contains(id));
                    if !selected && !own && self.acked_coverer(id).is_some() {
                        Status::Via
                    } else {
                        Status::On
                    }
                } else if self.model.master == Master::Paused && (selected || covered) {
                    Status::Paused
                } else {
                    Status::Off
                }
            }
        }
    }

    /// Whether `row`'s status still waits for an acknowledgement.
    pub(crate) fn pending(&self, row: Row) -> bool {
        if !self.model.phase.is_attached() {
            return false;
        }
        let Row::Toggle(id) = row else {
            let wanted = self.profile.effective(&self.desired()).bits();
            return row == Row::Master && self.model.acked != Some(wanted);
        };
        let bits = self.profile.toggle(id).controls.word();
        let effective = self.profile.effective(&self.desired()).bits().word();
        // Validation gives every toggle at least one bit, so this is never vacuously true.
        let wanted = effective & bits == bits;
        let shown = matches!(self.status(row), Status::On | Status::Via);
        wanted != shown
    }

    /// `row`'s label.
    pub(crate) fn label(&self, row: Row) -> &'p str {
        match row {
            Row::Master => "All cheats",
            Row::Toggle(id) => self.profile.toggle(id).label,
            Row::Option(id) => self.profile.option(id).label,
            Row::Sounds => "Chimes",
            Row::StartPaused => "Start paused",
        }
    }

    /// Whether `row` shows the selection marker.
    pub(crate) fn marked(&self, row: Row) -> bool {
        match row {
            Row::Master => self.model.phase.is_attached() && self.model.master == Master::Active,
            Row::Toggle(id) => self.model.settings.selection.contains(id),
            Row::Option(id) => self.model.settings.options.contains(id),
            Row::Sounds => self.model.settings.sounds,
            Row::StartPaused => self.model.settings.start_paused,
        }
    }

    /// `row`'s hotkey, if it can have one.
    pub(crate) fn binding(&self, row: Row) -> Option<Binding> {
        match row {
            Row::Master => Some(self.model.settings.bindings.get(Target::Master)),
            Row::Toggle(id) => Some(self.model.settings.bindings.get(Target::Toggle(id))),
            _ => None,
        }
    }

    /// The label of `target`.
    pub(crate) fn target_label(&self, target: Target) -> &'p str {
        match target {
            Target::Master => self.label(Row::Master),
            Target::Toggle(id) => self.label(Row::Toggle(id)),
        }
    }

    fn compute_title(&self) -> String {
        let game = text::sanitize(self.profile.profile().game.name);
        let state = view::phase_word(self);
        if self.model.phase.is_attached() {
            let master = self.status(Row::Master).word();
            format!("{game} trainer - {state} - {master}")
        } else {
            format!("{game} trainer - {state}")
        }
    }
}

#[cfg(test)]
mod tests;
