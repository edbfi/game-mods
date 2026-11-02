// SPDX-License-Identifier: AGPL-3.0-only
//! Drawing the dashboard: the wide two-pane screen, the compact tabs, the tiny strip, cards and dialogs.
#![forbid(unsafe_code)]

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

use edbfi_trainer::chord::Binding;
use edbfi_trainer::control::Master;
use edbfi_trainer::lifecycle::{Finish, Phase, SettingsProblem, VerifyStage};
use edbfi_trainer::profile::{Checked, HookKind, SiteId};
use edbfi_trainer::recovery::RecoveryAction;
use edbfi_trainer::settings::Target;

use crate::app::{App, Heading, Item, Last, Modal, Pane, Row, SaveState, Status};
use crate::layout::{Layout, Window};
use crate::text;
use crate::theme::Tone;
use crate::widgets::{Canvas, Para, Wrapped, wrap_paras};

/// The wide left pane's width, including its right margin; the divider follows it.
const LEFT: u16 = 48;
/// Lines under the wide left pane's rows: a blank, four detail lines and three file lines.
const LEFT_FOOT: usize = 8;
/// Columns of a row, from the pane's origin: label, hotkey (seven cells and a space), status.
const LABEL_X: u16 = 2;
const BINDING_X: u16 = 26;
const STATUS_X: u16 = 34;
const BINDING_W: u16 = 7;
/// The widest status word, `INACTIVE`.
const STATUS_W: u16 = 8;

const HELP_ACTIONS: &str = "Esc close  Up/Down scroll";
const WIDE_HELP: &str = "Up/Down move  Tab section  Space toggle  R bind  P pause  ? help  Q quit";
const COMPACT_HELP: [&str; 2] = [
    "Tab section  Arrows move  R bind",
    "Space toggle  P pause  ? help  Q quit",
];

impl Widget for &App<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut canvas = Canvas::new(buf, area, self.model.appearance);
        let (width, height) = (canvas.width(), canvas.height());
        match Layout::of(width, height) {
            Layout::Wide => wide(self, &mut canvas),
            Layout::Compact => compact(self, &mut canvas),
            Layout::Tiny => tiny(self, &mut canvas),
        }
        if Layout::of(width, height) != Layout::Tiny {
            if let Some(content) = card(self) {
                draw_card(self, &mut canvas, &content);
            }
            if let Some(modal) = &self.model.modal {
                draw_modal(self, &mut canvas, modal);
            }
        }
    }
}

// ----- geometry shared with the app -----

fn compact_help_lines(width: u16) -> usize {
    let one = 1 + text::width(COMPACT_HELP[0]) + 2 + text::width(COMPACT_HELP[1]);
    if usize::from(width) > one { 1 } else { 2 }
}

const fn compact_detail_lines(height: u16) -> usize {
    if height >= 17 {
        3
    } else if height >= 14 {
        2
    } else {
        1
    }
}

const fn compact_status_line(height: u16) -> bool {
    height >= 13
}

/// How many lines a pane's list has on the current screen.
pub(crate) fn pane_height(app: &App<'_>, pane: Pane) -> usize {
    let (width, height) = app.model.size;
    let body = usize::from(height).saturating_sub(5);
    match (Layout::of(width, height), pane) {
        (Layout::Wide, Pane::Left) => body.saturating_sub(LEFT_FOOT),
        (Layout::Wide, _) => body.saturating_sub(1),
        _ => usize::from(height)
            .saturating_sub(2)
            .saturating_sub(usize::from(compact_status_line(height)))
            .saturating_sub(compact_detail_lines(height))
            .saturating_sub(compact_help_lines(width)),
    }
}

/// The key line, or why keys wait.
fn footer<'a>(app: &App<'_>, keys: &'a str) -> &'a str {
    if app.input_closed() {
        "Keys wait until this finishes"
    } else {
        keys
    }
}

// ----- words -----

/// The phase in one or two words, for the title and the strip.
pub(crate) fn phase_word(app: &App<'_>) -> &'static str {
    match &app.model.phase {
        _ if app.model.quitting && !matches!(app.model.phase, Phase::Finished(_)) => "QUITTING",
        Phase::Settings(_) => "SETTINGS",
        Phase::Waiting => "WAITING",
        Phase::Verifying { .. } => "VERIFYING",
        Phase::Installing => "INSTALLING",
        Phase::Attached { .. } => "ATTACHED",
        Phase::Restoring => "RESTORING",
        Phase::Finished(_) => "DONE",
        Phase::Failed(failure) if failure.left_hooks() => "NOT RESTORED",
        Phase::Failed(_) => "NEEDS ATTENTION",
    }
}

const fn stage_text(stage: VerifyStage) -> &'static str {
    match stage {
        VerifyStage::Executable => "checking the executable",
        VerifyStage::Unpacking => "waiting for the code to unpack",
        VerifyStage::Code => "checking the game's code",
        VerifyStage::Sites => "checking hook sites",
    }
}

fn stage_number(stage: VerifyStage) -> usize {
    VerifyStage::ALL
        .iter()
        .position(|s| *s == stage)
        .map_or(0, |at| at + 1)
}

fn recovery_text(action: RecoveryAction) -> &'static str {
    match action {
        RecoveryAction::StartGame => "Start the game; the trainer connects by itself.",
        RecoveryAction::CloseDuplicates => "Close the extra copies of the game so only one runs.",
        RecoveryAction::Retry => "Try again.",
        RecoveryAction::EditSettings => "Fix the reported line in the settings file, then reload.",
        RecoveryAction::ResetSettings => {
            "This trainer can't read that file. Replace it with defaults; the old file is kept."
        }
        RecoveryAction::UpdateWindows => {
            "Update Windows; this version lacks something the trainer needs."
        }
        RecoveryAction::RestartGame => "Restart the game so its code is in a known state.",
        RecoveryAction::UnsupportedBuild => {
            "This game build isn't supported by this trainer version."
        }
        RecoveryAction::ModifiedGameCode => {
            "Something else changed the game's code, such as another tool. Restart the game without it."
        }
        RecoveryAction::CloseOtherTrainer => "Close the other trainer first.",
        _ => "This is a trainer bug. Please report it with the log.",
    }
}

const fn checked_word(checked: Checked) -> &'static str {
    match checked {
        Checked::Passed => "gameplay checked",
        Checked::Partial => "gameplay partial",
        Checked::Pending => "gameplay unchecked",
    }
}

fn binding_text(binding: Binding) -> String {
    binding
        .0
        .map_or_else(|| "No hotkey".to_owned(), |chord| chord.to_string())
}

/// The header's state, with its tone.
fn header_state(app: &App<'_>, dot: &str) -> Vec<(String, Tone)> {
    let sep = (format!(" {dot} "), Tone::Muted);
    let word = phase_word(app);
    match &app.model.phase {
        _ if app.model.quitting => vec![(word.into(), Tone::Accent)],
        Phase::Attached { .. } => {
            let status = app.status(Row::Master);
            let chord = app
                .model
                .settings
                .bindings
                .get(Target::Master)
                .0
                .map_or_else(|| "P".to_owned(), |chord| chord.to_string());
            let action = match app.model.master {
                Master::Active => "pauses",
                Master::Paused => "resumes",
            };
            vec![
                (word.into(), Tone::On),
                sep.clone(),
                (status.word().into(), status.tone()),
                sep,
                (format!("{chord} {action}"), Tone::Muted),
            ]
        }
        Phase::Waiting => vec![
            (word.into(), Tone::Armed),
            (
                format!(" for {}", app.profile.profile().game.exe),
                Tone::Text,
            ),
        ],
        Phase::Verifying { stage } => vec![
            (word.into(), Tone::Accent),
            sep,
            (
                format!(
                    "{}/{} {}",
                    stage_number(*stage),
                    VerifyStage::ALL.len(),
                    stage_text(*stage)
                ),
                Tone::Text,
            ),
        ],
        Phase::Installing | Phase::Restoring => vec![
            (word.into(), Tone::Accent),
            sep,
            ("input waits".into(), Tone::Muted),
        ],
        Phase::Settings(_) | Phase::Failed(_) => vec![(word.into(), Tone::Error)],
        Phase::Finished(_) => vec![(word.into(), Tone::Accent)],
    }
}

fn sites(app: &App<'_>, row: Row) -> Vec<SiteId> {
    let profile = app.profile.profile();
    let class_bits = profile
        .classes
        .iter()
        .fold(0u32, |bits, class| bits | class.bit.word());
    let derived: Vec<_> = match row {
        Row::Option(id) => {
            let key = app.profile.option(id).key;
            profile
                .derived
                .iter()
                .filter(|rule| rule.option == key)
                .map(|rule| rule.bit.word())
                .collect()
        }
        _ => Vec::new(),
    };
    let controls = match row {
        Row::Toggle(id) => app.profile.toggle(id).controls.word(),
        _ => 0,
    };
    profile
        .sites
        .iter()
        .filter(|site| match site.kind {
            HookKind::Gate { bit, .. } | HookKind::Pressure { bit, .. } => {
                controls & bit.word() != 0 || derived.contains(&bit.word())
            }
            HookKind::NoReload(_) => controls & class_bits != 0,
        })
        .map(|site| site.id)
        .collect()
}

/// The detail block for the focused row: a title, a hint and one summary line of short facts.
fn details(app: &App<'_>, row: Row) -> (String, String, Vec<(String, Tone)>) {
    let label = app.label(row).to_owned();
    let mut facts: Vec<(String, Tone)> = Vec::new();
    if let Some(binding) = app.binding(row) {
        let tone = if binding.0.is_some() {
            Tone::Text
        } else {
            Tone::Muted
        };
        facts.push((binding_text(binding), tone));
    }
    let hint = match row {
        Row::Master => {
            facts.push((
                if app.model.settings.start_paused {
                    "starts paused".into()
                } else {
                    "starts active".into()
                },
                Tone::Muted,
            ));
            "Pauses or resumes every cheat; your selections are kept.".to_owned()
        }
        Row::Toggle(id) => {
            let toggle = app.profile.toggle(id);
            match app.status(row) {
                Status::Via => {
                    if let Some(cover) = app.acked_coverer(id) {
                        facts.push((format!("via {}", app.label(Row::Toggle(cover))), Tone::Via));
                    }
                }
                _ if !app.model.phase.is_attached()
                    && app.model.settings.selection.contains(id)
                    && !app.input_closed() =>
                {
                    facts.push(("saved; applies once hooked".into(), Tone::Armed));
                }
                _ => {}
            }
            if app.pending(row) {
                facts.push(("waiting for the game".into(), Tone::Armed));
            }
            toggle.hint.to_owned()
        }
        Row::Option(id) => {
            let key = app.profile.option(id).key;
            let triggers: Vec<&str> = app
                .profile
                .profile()
                .derived
                .iter()
                .filter(|rule| rule.option == key)
                .flat_map(|rule| rule.when_any.iter())
                .filter_map(|key| app.profile.toggle_id(key))
                .map(|id| app.label(Row::Toggle(id)))
                .collect();
            if let Some((last, rest)) = triggers.split_last() {
                let list = if rest.is_empty() {
                    (*last).to_owned()
                } else {
                    format!("{} or {last}", rest.join(", "))
                };
                facts.push((format!("applies with {list}"), Tone::Muted));
            }
            app.profile.option(id).hint.to_owned()
        }
        Row::Sounds => "A chime plays when the cheats in the game change.".to_owned(),
        Row::StartPaused => "Connect with every cheat paused; press P to resume.".to_owned(),
    };
    if let Row::Toggle(id) = row {
        facts.push((
            checked_word(app.profile.toggle(id).gameplay).into(),
            Tone::Muted,
        ));
    }
    // Hook sites matter least to a player, so they go last and are the first to be left out.
    let ids = sites(app, row);
    if !ids.is_empty() {
        let list: Vec<String> = ids.iter().map(ToString::to_string).collect();
        facts.push((list.join(" "), Tone::Muted));
    }
    (label, hint, facts)
}

/// Draws `facts` separated by dots from column `x` of row `y`, within `max` cells. After the first, a fact
/// that does not fit whole is left out with everything after it, so the line never ends mid-word.
fn draw_facts(canvas: &mut Canvas<'_>, x: u16, y: u16, max: u16, facts: &[(String, Tone)]) {
    let dot = format!(" {} ", canvas.glyphs().dot);
    let end = x.saturating_add(max);
    let mut at = x;
    for (i, (fact, tone)) in facts.iter().enumerate() {
        if i > 0 {
            if usize::from(end.saturating_sub(at)) < text::width(&dot) + text::width(fact) {
                return;
            }
            at = canvas.say(at, y, &dot, end.saturating_sub(at), Tone::Muted);
        }
        at = canvas.say(at, y, fact, end.saturating_sub(at), *tone);
    }
}

// ----- rows -----

/// Where a list row's parts go, relative to the canvas.
#[derive(Clone, Copy)]
struct Columns {
    focus: u16,
    label: u16,
    binding: Option<u16>,
    status: u16,
    end: u16,
}

fn draw_row(app: &App<'_>, canvas: &mut Canvas<'_>, y: u16, row: Row, columns: Columns) {
    let glyphs = canvas.glyphs();
    let focused = row == app.model.focus && app.model.modal.is_none();
    if focused {
        canvas.say(columns.focus, y, glyphs.focus, 1, Tone::Focus);
    }
    if app.marked(row) {
        canvas.say(columns.focus + 1, y, glyphs.selected, 1, Tone::Accent);
    }
    let label_end = columns.binding.unwrap_or(columns.status).saturating_sub(1);
    let label_tone = if focused { Tone::Focus } else { Tone::Text };
    canvas.say(
        columns.label,
        y,
        app.label(row),
        label_end.saturating_sub(columns.label),
        label_tone,
    );
    if let (Some(x), Some(binding)) = (columns.binding, app.binding(row))
        && let Some(chord) = binding.0
    {
        canvas.say(x, y, &chord.to_string(), BINDING_W, Tone::Text);
    }
    let status = app.status(row);
    canvas.say(
        columns.status,
        y,
        status.word(),
        columns.end.saturating_sub(columns.status),
        status.tone(),
    );
}

/// Draws `items` in `height` lines from row `y`, scrolled to keep the focus visible.
fn draw_items(
    app: &App<'_>,
    canvas: &mut Canvas<'_>,
    pane: Pane,
    items: &[Item],
    y: u16,
    height: usize,
    columns: Columns,
) {
    let focus = items
        .iter()
        .position(|item| *item == Item::Row(app.model.focus));
    let top = app.model.scroll.get(&pane).copied().unwrap_or(0);
    let window = Window::follow(items.len(), height, top, focus);
    let glyphs = canvas.glyphs();
    let mut line = y;
    if window.above {
        let text = format!("{} {} more", glyphs.more_above, window.top);
        canvas.say(
            columns.label,
            line,
            &text,
            columns.end.saturating_sub(columns.label),
            Tone::Muted,
        );
        line += 1;
    }
    for item in &items[window.top..window.top + window.shown] {
        match item {
            Item::Heading(heading) => {
                let title = match heading {
                    Heading::Trainer => "TRAINER".to_owned(),
                    Heading::Options => "OPTIONS".to_owned(),
                    Heading::Group(index) => app.groups[*index].title.to_uppercase(),
                };
                canvas.say(
                    columns.label,
                    line,
                    &title,
                    columns.end.saturating_sub(columns.label),
                    Tone::Accent,
                );
            }
            Item::Blank => {}
            Item::Row(row) => draw_row(app, canvas, line, *row, columns),
        }
        line += 1;
    }
    if window.below {
        let hidden = items.len() - window.top - window.shown;
        let text = format!("{} {hidden} more", glyphs.more_below);
        canvas.say(
            columns.label,
            line,
            &text,
            columns.end.saturating_sub(columns.label),
            Tone::Muted,
        );
    }
}

// ----- screens -----

fn wide(app: &App<'_>, canvas: &mut Canvas<'_>) {
    let (width, height) = (canvas.width(), canvas.height());
    // Header.
    let end = canvas.say(1, 0, "edbfi", 5, Tone::Accent);
    let dot = canvas.glyphs().dot;
    let end = canvas.say(end, 0, &format!(" {dot} "), 3, Tone::Muted);
    let name_end = canvas.say(end, 0, app.profile.profile().game.name, LEFT, Tone::Text);
    draw_state(app, canvas, name_end + 2, width - 1, 0);
    canvas.rule(1, 1, width - 2);
    if card(app).is_some() {
        return;
    }
    // Panes.
    let body = height - 5;
    let panes = app.panes();
    for (pane, items) in &panes {
        match pane {
            Pane::Left => {
                let columns = Columns {
                    focus: 0,
                    label: LABEL_X,
                    binding: Some(BINDING_X),
                    status: STATUS_X,
                    end: LEFT - 1,
                };
                draw_items(
                    app,
                    canvas,
                    *pane,
                    items,
                    2,
                    pane_height(app, *pane),
                    columns,
                );
            }
            _ => {
                let x = LEFT + 1;
                let status = width - 1 - STATUS_W;
                let columns = Columns {
                    focus: x + 1,
                    label: x + 3,
                    binding: Some(status - BINDING_W - 1),
                    status,
                    end: width - 1,
                };
                if let Some(group) = app.groups.iter().find(|group| group.classes) {
                    let title = group.title.to_uppercase();
                    canvas.say(x + 2, 2, &title, width - x - 3, Tone::Accent);
                }
                draw_items(
                    app,
                    canvas,
                    *pane,
                    items,
                    3,
                    pane_height(app, *pane),
                    columns,
                );
            }
        }
    }
    if panes.len() > 1 {
        let bar = canvas.glyphs().bar;
        for y in 2..2 + body {
            canvas.say(LEFT, y, bar, 1, Tone::Muted);
        }
    }
    // Details and files under the left pane.
    let rows = u16::try_from(pane_height(app, Pane::Left)).unwrap_or(0);
    let y = 2 + rows + 1;
    let room = LEFT - 3;
    let (label, hint, facts) = details(app, app.model.focus);
    canvas.say(2, y, &label, room, Tone::Accent);
    let hint = text::wrap(&text::sanitize(&hint), usize::from(room));
    let mut line = y + 1;
    for (i, part) in hint.iter().take(2).enumerate() {
        let part = if i == 1 && hint.len() > 2 {
            format!("{part}{}", canvas.glyphs().ellipsis)
        } else {
            part.clone()
        };
        canvas.say(2, line, &part, room, Tone::Text);
        line += 1;
    }
    draw_facts(canvas, 2, line, room, &facts);
    let y = y + 4;
    draw_files(app, canvas, 2, y, room);
    // Footer.
    canvas.rule(1, height - 3, width - 2);
    draw_status(app, canvas, 1, height - 2, width - 2);
    canvas.say(
        1,
        height - 1,
        footer(app, WIDE_HELP),
        width - 2,
        Tone::Muted,
    );
}

fn draw_state(app: &App<'_>, canvas: &mut Canvas<'_>, left: u16, right: u16, y: u16) {
    let mut parts = header_state(app, app.model.appearance.glyphs().dot);
    let room = usize::from(right.saturating_sub(left));
    // Drop trailing details, each with its separator, until the rest fits; the phase word always stays.
    while parts.len() > 1
        && parts
            .iter()
            .map(|(text, _)| text::width(text))
            .sum::<usize>()
            > room
    {
        parts.truncate(parts.len().saturating_sub(2).max(1));
    }
    let total: usize = parts.iter().map(|(text, _)| text::width(text)).sum();
    if total > room {
        if let Some((word, tone)) = parts.first() {
            let style = canvas.style(*tone);
            canvas.put_right(left, right, y, word, style);
        }
        return;
    }
    let mut x = right - u16::try_from(total).unwrap_or(0);
    for (text, tone) in &parts {
        x = canvas.say(x, y, text, right.saturating_sub(x), *tone);
    }
}

fn draw_files(app: &App<'_>, canvas: &mut Canvas<'_>, x: u16, y: u16, room: u16) {
    let ellipsis = canvas.glyphs().ellipsis;
    let path_room = usize::from(room.saturating_sub(9));
    let file = |canvas: &mut Canvas<'_>,
                y: u16,
                name: &str,
                path: Option<&std::path::Path>,
                missing: &str| {
        let end = canvas.say(x, y, name, 9, Tone::Muted);
        let end = end.max(x + 9);
        match path {
            Some(path) => {
                let shown = text::fit_start(
                    &text::sanitize(&path.display().to_string()),
                    path_room,
                    ellipsis,
                );
                canvas.say(end, y, &shown, room - 9, Tone::Text);
            }
            None => {
                canvas.say(end, y, missing, room - 9, Tone::Muted);
            }
        }
    };
    file(
        canvas,
        y,
        "Settings",
        app.model.paths.settings.as_deref(),
        "not saved this session",
    );
    file(
        canvas,
        y + 1,
        "Log",
        app.model.paths.log.as_deref(),
        "not written",
    );
    let info = match &app.model.phase {
        Phase::Attached { attach_duration } => {
            format!("Attached in {} ms", attach_duration.as_millis())
        }
        Phase::Waiting => "Start the game; saved cheats apply on hook.".into(),
        Phase::Verifying { .. } => "Nothing is changed until every check passes.".into(),
        Phase::Installing => "Installing hooks with every cheat off.".into(),
        Phase::Restoring => "Restoring the game's code.".into(),
        _ => String::new(),
    };
    canvas.say(x, y + 2, &info, room, Tone::Muted);
}

/// The status line: a notice, or the last change and whether it is saved.
fn draw_status(app: &App<'_>, canvas: &mut Canvas<'_>, x: u16, y: u16, room: u16) {
    let end = x + room;
    if let Some(notice) = &app.model.notice {
        canvas.say(x, y, &notice.text, room, notice.tone);
        return;
    }
    let Some(last) = &app.model.last else {
        if app.model.save == SaveState::SessionOnly {
            canvas.say(
                x,
                y,
                "Settings are not saved this session",
                room,
                Tone::Muted,
            );
        }
        return;
    };
    let mut at = canvas.say(x, y, "Last: ", room, Tone::Muted);
    let saves = match last {
        Last::Row(row) => {
            at = canvas.say(at, y, app.label(*row), end.saturating_sub(at), Tone::Text);
            let status = app.status(*row);
            at = canvas.say(at, y, " ", 1, Tone::Text);
            at = canvas.say(at, y, status.word(), end.saturating_sub(at), status.tone());
            if app.pending(*row) {
                let dot = format!(" {} ", canvas.glyphs().dot);
                at = canvas.say(at, y, &dot, end.saturating_sub(at), Tone::Muted);
                at = canvas.say(
                    at,
                    y,
                    "waiting for the game",
                    end.saturating_sub(at),
                    Tone::Armed,
                );
            }
            *row != Row::Master
        }
        Last::Bound(target) => {
            let binding = app.model.settings.bindings.get(*target);
            at = canvas.say(
                at,
                y,
                app.target_label(*target),
                end.saturating_sub(at),
                Tone::Text,
            );
            at = canvas.say(at, y, " hotkey ", end.saturating_sub(at), Tone::Muted);
            at = canvas.say(
                at,
                y,
                &binding_text(binding),
                end.saturating_sub(at),
                Tone::Text,
            );
            true
        }
        Last::Message { text, tone } => {
            at = canvas.say(at, y, text, end.saturating_sub(at), *tone);
            false
        }
    };
    if saves || app.model.save == SaveState::SessionOnly {
        let (word, tone) = match app.model.save {
            SaveState::Idle => return,
            SaveState::Pending => ("saving", Tone::Muted),
            SaveState::Saved => ("saved", Tone::Muted),
            SaveState::Failed => ("not saved", Tone::Error),
            SaveState::SessionOnly => ("not saved this session", Tone::Muted),
        };
        let dot = format!(" {} ", canvas.glyphs().dot);
        at = canvas.say(at, y, &dot, end.saturating_sub(at), Tone::Muted);
        canvas.say(at, y, word, end.saturating_sub(at), tone);
    }
}

fn compact(app: &App<'_>, canvas: &mut Canvas<'_>) {
    let (width, height) = (canvas.width(), canvas.height());
    let name_end = canvas.say(
        1,
        0,
        app.profile.profile().game.name,
        width / 2,
        Tone::Accent,
    );
    draw_state(app, canvas, name_end + 1, width - 1, 0);
    if card(app).is_some() {
        return;
    }
    // Tabs.
    let panes = app.panes();
    let (focused, _, _) = app.focused_pane();
    let titles: Vec<(&str, bool)> = panes
        .iter()
        .map(|(pane, _)| {
            let title = match pane {
                Pane::Tab(index) if *index < app.groups.len() => app.groups[*index].title,
                _ => "Options",
            };
            (title, *pane == focused)
        })
        .collect();
    let full: usize = titles
        .iter()
        .map(|(t, _)| text::width(t) + 1)
        .sum::<usize>()
        + 2;
    if full < usize::from(width) {
        let mut x = 1;
        for (title, current) in &titles {
            if *current {
                x = canvas.say(x, 1, &format!("[{title}]"), width - x, Tone::Accent);
            } else {
                x = canvas.say(x, 1, title, width - x, Tone::Muted);
            }
            x = canvas.say(x, 1, " ", 1, Tone::Muted);
        }
    } else if let Some(at) = titles.iter().position(|(_, current)| *current) {
        let x = canvas.say(
            1,
            1,
            &format!("[{}]", titles[at].0),
            width - 10,
            Tone::Accent,
        );
        canvas.say(
            x + 1,
            1,
            &format!("{}/{}", at + 1, titles.len()),
            6,
            Tone::Muted,
        );
    }
    // The focused tab's rows.
    let status = width - 1 - STATUS_W;
    let binding = (width >= 56).then(|| status - BINDING_W - 2);
    let columns = Columns {
        focus: 1,
        label: 3,
        binding,
        status,
        end: width - 1,
    };
    let list = pane_height(app, focused);
    if let Some((_, items)) = panes.iter().find(|(pane, _)| *pane == focused) {
        draw_items(app, canvas, focused, items, 2, list, columns);
    }
    let mut y = 2 + u16::try_from(list).unwrap_or(0);
    let room = width - 2;
    let notice = app.model.notice.as_ref();
    if compact_status_line(height) {
        draw_status(app, canvas, 1, y, room);
        y += 1;
    }
    let (_, hint, mut facts) = details(app, app.model.focus);
    if !compact_status_line(height) && app.model.save == SaveState::SessionOnly {
        facts.insert(0, ("not saved".into(), Tone::Armed));
    }
    let lines = compact_detail_lines(height);
    match notice {
        Some(notice) if !compact_status_line(height) => {
            canvas.say(1, y, &notice.text, room, notice.tone);
        }
        _ => draw_facts(canvas, 1, y, room, &facts),
    }
    let hint = text::wrap(&text::sanitize(&hint), usize::from(room));
    let shown = lines - 1;
    for (i, part) in hint.iter().take(shown).enumerate() {
        let part = if i + 1 == shown && hint.len() > shown {
            format!("{part}{}", canvas.glyphs().ellipsis)
        } else {
            part.clone()
        };
        canvas.say(
            1,
            y + 1 + u16::try_from(i).unwrap_or(0),
            &part,
            room,
            Tone::Text,
        );
    }
    // A dialog covers the list; its own keys are in its box.
    if app.model.modal.is_some() {
        return;
    }
    let y = height - u16::try_from(compact_help_lines(width)).unwrap_or(2);
    if app.input_closed() {
        canvas.say(1, y, footer(app, ""), room, Tone::Muted);
    } else if compact_help_lines(width) == 1 {
        let line = format!("{}  {}", COMPACT_HELP[0], COMPACT_HELP[1]);
        canvas.say(1, y, &line, room, Tone::Muted);
    } else {
        canvas.say(1, y, COMPACT_HELP[0], room, Tone::Muted);
        canvas.say(1, y + 1, COMPACT_HELP[1], room, Tone::Muted);
    }
}

fn tiny(app: &App<'_>, canvas: &mut Canvas<'_>) {
    let width = canvas.width();
    if width == 0 {
        return;
    }
    let game = app.profile.profile().game.name;
    let (state, tone) = match &app.model.phase {
        Phase::Attached { .. } if !app.model.quitting => {
            let status = app.status(Row::Master);
            (status.word(), status.tone())
        }
        _ => {
            let tone = header_state(app, "-")
                .first()
                .map_or(Tone::Text, |(_, tone)| *tone);
            (phase_word(app), tone)
        }
    };
    let end = canvas.say(
        0,
        0,
        game,
        width.saturating_sub(text::width(state) as u16 + 3).max(1),
        Tone::Accent,
    );
    let end = canvas.say(end, 0, " | ", width.saturating_sub(end), Tone::Muted);
    canvas.say(end, 0, state, width.saturating_sub(end), tone);
    let second = match &app.model.phase {
        _ if app.model.modal.is_some() => {
            ("A dialog is open; Esc closes it".to_owned(), Tone::Armed)
        }
        Phase::Settings(problem) => (recovery_text(problem.recovery()).to_owned(), Tone::Error),
        Phase::Failed(failure) => (recovery_text(failure.recovery).to_owned(), Tone::Error),
        _ => {
            let row = app.model.focus;
            let status = app.status(row);
            let count = app
                .profile
                .toggles()
                .filter(|(id, _)| app.model.settings.selection.contains(*id))
                .count();
            (
                format!(
                    "{} {} {} {count} selected",
                    app.label(row),
                    status.word(),
                    canvas.glyphs().dot
                ),
                Tone::Text,
            )
        }
    };
    canvas.say(0, 1, &second.0, width, second.1);
    let (min_w, min_h) = Layout::COMPACT;
    canvas.say(
        0,
        2,
        &format!("Grow to {min_w}x{min_h}"),
        width,
        Tone::Muted,
    );
}

/// The header's state as plain text.
pub(crate) fn plain_state(app: &App<'_>) -> String {
    header_state(app, "-")
        .into_iter()
        .map(|(text, _)| text)
        .collect()
}

/// The current card as plain lines: its title, paragraphs and actions.
pub(crate) fn plain_card(app: &App<'_>) -> Option<Vec<String>> {
    card(app).map(|content| {
        std::iter::once(content.title)
            .chain(
                content
                    .paras
                    .into_iter()
                    .filter(|para| !para.text.is_empty())
                    .map(|para| format!("  {}", para.text)),
            )
            // A lost console reads no keys; only Ctrl+C still reaches the trainer.
            .chain((!content.actions.is_empty()).then(|| {
                if app.plain() {
                    "  Ctrl+C quits.".to_owned()
                } else {
                    format!("  Keys: {}", content.actions)
                }
            }))
            .collect()
    })
}

// ----- cards -----

/// A card: a title, wrapped paragraphs and a pinned line of actions.
pub(crate) struct Card {
    title: String,
    tone: Tone,
    paras: Vec<Para>,
    actions: String,
}

fn card(app: &App<'_>) -> Option<Card> {
    let log = || {
        app.model.paths.log.as_ref().map_or_else(
            || Para::new("No log file is being written.", Tone::Muted),
            |path| Para::new(format!("Log: {}", path.display()), Tone::Muted),
        )
    };
    match &app.model.phase {
        Phase::Settings(SettingsProblem::Invalid { path, error }) => Some(Card {
            title: "Settings need attention".into(),
            tone: Tone::Error,
            paras: vec![
                Para::new(error.to_string(), Tone::Error),
                Para::new(recovery_text(error.recovery()), Tone::Text),
                Para::new(
                    "D replaces the file with defaults and keeps the old one as a backup. Nothing connects until then.",
                    Tone::Muted,
                ),
                Para::blank(),
                Para::new(path.display().to_string(), Tone::Muted),
            ],
            actions: "Enter reload  D defaults  Q quit".into(),
        }),
        Phase::Settings(SettingsProblem::Store {
            message,
            recovery,
            session_only,
            ..
        }) => {
            let mut paras = vec![
                Para::new(message.clone(), Tone::Error),
                Para::new(recovery_text(*recovery), Tone::Text),
            ];
            if *session_only {
                paras.push(Para::new(
                    "No settings file exists yet, so S can continue with defaults that won't be saved.",
                    Tone::Muted,
                ));
            }
            paras.push(Para::new("Nothing connects until then.", Tone::Muted));
            Some(Card {
                title: "Settings unavailable".into(),
                tone: Tone::Error,
                paras,
                actions: if *session_only {
                    "Enter retry  S unsaved  Q quit".into()
                } else {
                    "Enter retry  Q quit".into()
                },
            })
        }
        Phase::Failed(failure) if failure.left_hooks() => {
            let ids: Vec<String> = failure.unrestored.iter().map(ToString::to_string).collect();
            Some(Card {
                title: "Game code not restored".into(),
                tone: Tone::Error,
                paras: vec![
                    Para::new(
                        "Restart the game before you play on. Every cheat is off, but its code still holds trainer hooks, and the trainer won't reconnect to it.",
                        Tone::Text,
                    ),
                    Para::new(
                        format!("Sites still changed: {}", ids.join(" ")),
                        Tone::Error,
                    ),
                    Para::blank(),
                    Para::new(failure.message.clone(), Tone::Muted),
                    log(),
                ],
                actions: "Q quit".into(),
            })
        }
        Phase::Failed(failure) => Some(Card {
            title: "Needs attention".into(),
            tone: Tone::Error,
            paras: vec![
                Para::new(failure.message.clone(), Tone::Error),
                Para::new(recovery_text(failure.recovery), Tone::Text),
                Para::blank(),
                log(),
            ],
            actions: if failure.can_retry() {
                "Enter retry  Q quit".into()
            } else {
                "Q quit".into()
            },
        }),
        Phase::Finished(finish) => Some(Card {
            title: "Done".into(),
            tone: Tone::Accent,
            paras: vec![Para::new(
                match finish {
                    Finish::Untouched => "The game was not changed.",
                    Finish::Restored => "The game's code is restored and no cheats remain.",
                },
                Tone::Text,
            )],
            actions: String::new(),
        }),
        _ => None,
    }
}

/// The card's box on a `width` × `height` screen: below the header, at most 76 wide.
fn card_area(width: u16, height: u16) -> Rect {
    let top = if Layout::of(width, height) == Layout::Wide {
        2
    } else {
        1
    };
    let w = width.saturating_sub(2).min(76);
    Rect::new((width - w) / 2, top, w, height.saturating_sub(top))
}

/// Rows a box spends on anything but its text: the borders, and the actions with a spacer above them when
/// the box is tall enough.
fn box_chrome(height: u16, actions: &str) -> usize {
    match (actions.is_empty(), height >= 12) {
        (true, _) => 2,
        (false, true) => 4,
        (false, false) => 3,
    }
}

/// The card's wrapped lines and how many fit.
fn card_lines(app: &App<'_>, content: &Card) -> (Vec<Wrapped>, usize) {
    let (width, height) = app.model.size;
    let area = card_area(width, height);
    let lines = wrap_paras(&content.paras, usize::from(area.width.saturating_sub(4)));
    let visible =
        usize::from(area.height).saturating_sub(box_chrome(area.height, &content.actions));
    (lines, visible)
}

/// How far the current card can scroll.
pub(crate) fn card_scroll_max(app: &App<'_>) -> usize {
    card(app).map_or(0, |content| {
        let (lines, visible) = card_lines(app, &content);
        Window::last_top(lines.len(), visible)
    })
}

fn draw_card(app: &App<'_>, canvas: &mut Canvas<'_>, content: &Card) {
    let full = card_area(canvas.width(), canvas.height());
    let lines = wrap_paras(&content.paras, usize::from(full.width.saturating_sub(4)));
    let chrome = box_chrome(full.height, &content.actions);
    let visible = usize::from(full.height).saturating_sub(chrome);
    let height = u16::try_from(lines.len().min(visible) + chrome).unwrap_or(full.height);
    let area = Rect::new(full.x, full.y, full.width, height.min(full.height));
    draw_box(
        canvas,
        area,
        &content.title,
        content.tone,
        &lines,
        app.model.card_scroll,
        &content.actions,
    );
}

/// A bordered box with wrapped lines scrolled from `scroll` and `actions` pinned on its last inner row.
fn draw_box(
    canvas: &mut Canvas<'_>,
    area: Rect,
    title: &str,
    tone: Tone,
    lines: &[Wrapped],
    scroll: usize,
    actions: &str,
) {
    if area.width < 6 || area.height < 3 {
        return;
    }
    canvas.frame(area, title, tone);
    let inner = area.width - 4;
    let visible = usize::from(area.height).saturating_sub(box_chrome(area.height, actions));
    let window = Window::at(
        lines.len(),
        visible,
        scroll.min(Window::last_top(lines.len(), visible)),
    );
    let glyphs = canvas.glyphs();
    let mut y = area.y + 1;
    if window.above {
        canvas.say(
            area.x + 2,
            y,
            &format!("{} {} more", glyphs.more_above, window.top),
            inner,
            Tone::Muted,
        );
        y += 1;
    }
    for (line, tone) in &lines[window.top..window.top + window.shown] {
        canvas.say(area.x + 2, y, line, inner, *tone);
        y += 1;
    }
    if window.below {
        let hidden = lines.len() - window.top - window.shown;
        canvas.say(
            area.x + 2,
            y,
            &format!("{} {hidden} more", glyphs.more_below),
            inner,
            Tone::Muted,
        );
    }
    if !actions.is_empty() {
        canvas.say(
            area.x + 2,
            area.y + area.height - 2,
            actions,
            inner,
            Tone::Accent,
        );
    }
}

// ----- dialogs -----

/// A dialog's width: at most 64 on a wide screen, the whole width otherwise.
fn modal_width(width: u16, height: u16) -> u16 {
    if Layout::of(width, height) == Layout::Wide {
        width.saturating_sub(4).min(64)
    } else {
        width
    }
}

fn help_paras(app: &App<'_>) -> Vec<Para> {
    let mut paras = vec![
        Para::new("Keys", Tone::Accent),
        Para::new("Up/Down       move", Tone::Text),
        Para::new("Tab, Left/Right  switch section", Tone::Text),
        Para::new("Home/End, PgUp/PgDn  jump", Tone::Text),
        Para::new("Space/Enter   toggle the row", Tone::Text),
        Para::new("R             set the row's hotkey", Tone::Text),
        Para::new("P             pause or resume every cheat", Tone::Text),
        Para::new("? or H        this help", Tone::Text),
        Para::new("Q/Esc         quit", Tone::Text),
        Para::new("Hotkeys work in the game and here.", Tone::Muted),
        Para::blank(),
        Para::new("Status", Tone::Accent),
        Para::new("ON      applied in the game", Tone::On),
        Para::new("VIA     applied through another selected cheat", Tone::Via),
        Para::new("PAUSED  selected; every cheat is paused", Tone::Paused),
        Para::new("OFF     not applied", Tone::Muted),
        Para::new(
            "Cheats unlock once the trainer has hooked the game; a marked row is a saved choice and applies then.",
            Tone::Muted,
        ),
        Para::blank(),
        Para::new("Files", Tone::Accent),
    ];
    if let Some(reason) = &app.model.save_error {
        paras.push(Para::new(
            format!("The last save failed: {reason}. S retries."),
            Tone::Error,
        ));
    }
    if let Some(kept) = &app.model.kept {
        paras.push(Para::new(
            format!(
                "Your outside edit of the settings was kept: {}",
                kept.display()
            ),
            Tone::Armed,
        ));
    }
    paras.push(match &app.model.paths.settings {
        Some(path) => Para::new(format!("Settings: {}", path.display()), Tone::Text),
        None => Para::new("Settings: not saved this session", Tone::Muted),
    });
    paras.push(match &app.model.paths.log {
        Some(path) => Para::new(format!("Log: {}", path.display()), Tone::Text),
        None => Para::new("Log: not written", Tone::Muted),
    });
    paras
}

/// A dialog's box for `lines` of text, centred; on a compact screen it may use every row.
fn dialog_area(width: u16, height: u16, lines: usize, actions: &str) -> Rect {
    let w = modal_width(width, height);
    let room = if Layout::of(width, height) == Layout::Wide {
        height.saturating_sub(2)
    } else {
        height
    };
    let chrome = box_chrome(room, actions);
    let h = u16::try_from(lines + chrome).unwrap_or(u16::MAX).min(room);
    Rect::new((width - w) / 2, (height - h) / 2, w, h)
}

/// How far the help can scroll.
pub(crate) fn help_scroll_max(app: &App<'_>) -> usize {
    let (width, height) = app.model.size;
    let lines = wrap_paras(
        &help_paras(app),
        usize::from(modal_width(width, height).saturating_sub(4)),
    );
    let area = dialog_area(width, height, lines.len(), HELP_ACTIONS);
    Window::last_top(
        lines.len(),
        usize::from(area.height).saturating_sub(box_chrome(area.height, HELP_ACTIONS)),
    )
}

fn draw_modal(app: &App<'_>, canvas: &mut Canvas<'_>, modal: &Modal) {
    let (width, height) = (canvas.width(), canvas.height());
    let inner = usize::from(modal_width(width, height).saturating_sub(4));
    let (title, tone, paras, scroll, actions) = match modal {
        Modal::Help { scroll } => (
            "Help".to_owned(),
            Tone::Accent,
            help_paras(app),
            *scroll,
            HELP_ACTIONS.to_owned(),
        ),
        Modal::Capture {
            target,
            candidate,
            refusal,
        } => {
            let mut paras = Vec::new();
            if let Some(refusal) = refusal {
                paras.push(Para::new(refusal.clone(), Tone::Error));
            }
            paras.push(Para::new(
                format!(
                    "{}: {}",
                    app.target_label(*target),
                    binding_text(app.model.settings.bindings.get(*target))
                ),
                Tone::Text,
            ));
            let actions = if let Some(candidate) = candidate {
                paras.push(Para::new(
                    format!("New: {}", binding_text(*candidate)),
                    Tone::Armed,
                ));
                "Enter set  Esc cancel  Bksp remove"
            } else {
                paras.push(Para::new(
                    "Press a hotkey. Letters and digits need Ctrl or Alt.",
                    Tone::Muted,
                ));
                "Esc cancel  Backspace remove"
            };
            (
                "Set a hotkey".to_owned(),
                Tone::Accent,
                paras,
                0,
                actions.to_owned(),
            )
        }
        Modal::Conflict {
            target,
            chord,
            holder,
        } => {
            let old = app.model.settings.bindings.get(*target);
            let paras = vec![
                Para::new(
                    format!("{chord} already belongs to {}.", app.target_label(*holder)),
                    Tone::Text,
                ),
                Para::blank(),
                Para::new(
                    format!(
                        "Swap them? {} gets {chord}; {} gets {}.",
                        app.target_label(*target),
                        app.target_label(*holder),
                        if old.0.is_none() {
                            "no hotkey".to_owned()
                        } else {
                            binding_text(old)
                        }
                    ),
                    Tone::Text,
                ),
            ];
            (
                "Hotkey in use".to_owned(),
                Tone::Armed,
                paras,
                0,
                "Enter swap  Esc cancel".to_owned(),
            )
        }
        Modal::Quit { interrupted, .. } => {
            // The buttons are drawn on the blank line after the text, below.
            let mut paras = vec![
                Para::new(
                    "Every cheat turns off and the game's code is restored. The game keeps running.",
                    Tone::Text,
                ),
                Para::blank(),
                Para::blank(),
            ];
            if *interrupted {
                paras.push(Para::new("Ctrl+C again quits.", Tone::Muted));
            }
            (
                "Quit the trainer?".to_owned(),
                Tone::Armed,
                paras,
                0,
                "Enter choose  Esc stay".to_owned(),
            )
        }
    };
    let lines = wrap_paras(&paras, inner);
    let area = dialog_area(width, height, lines.len(), &actions);
    let scroll = scroll.min(Window::last_top(
        lines.len(),
        usize::from(area.height).saturating_sub(box_chrome(area.height, &actions)),
    ));
    draw_box(canvas, area, &title, tone, &lines, scroll, &actions);
    if let Modal::Quit { quit, .. } = modal {
        let row = wrap_paras(&paras[..2], inner).len();
        let y = area.y + 1 + u16::try_from(row).unwrap_or(0);
        let focus = canvas.glyphs().focus;
        let mut x = area.x + 2;
        for (label, chosen) in [("Stay", !*quit), ("Quit", *quit)] {
            let button = if chosen {
                format!("{focus} {label} ")
            } else {
                format!("  {label} ")
            };
            let style = if chosen {
                canvas
                    .style(Tone::Focus)
                    .add_modifier(ratatui::style::Modifier::REVERSED)
            } else {
                canvas.style(Tone::Text)
            };
            let end = canvas.put(
                x,
                y,
                &button,
                area.width.saturating_sub(x - area.x + 2),
                style,
            );
            x = end + 3;
        }
    }
}
