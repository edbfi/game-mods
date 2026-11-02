// SPDX-License-Identifier: AGPL-3.0-only
//! The dashboard as plain ASCII lines, for redirected output and a lost console.
//!
//! The first report lists everything: the state, both files and every row. Later reports list only what
//! changed. No escape sequence or colour is ever written, and every non-ASCII character becomes one `?`.
#![forbid(unsafe_code)]

use crate::app::{App, Item, Last, Row};
use crate::text;

/// What a report showed, to find changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Shown {
    state: String,
    card: Option<Vec<String>>,
    rows: Vec<(String, String)>,
    status: Option<String>,
}

/// Turns the dashboard into lines of plain text.
#[derive(Clone, Debug, Default)]
pub struct Report {
    shown: Option<Shown>,
}

impl Report {
    /// A report that has shown nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The lines to print now: everything the first time, then only changes. Empty when nothing changed.
    pub fn lines(&mut self, app: &App<'_>) -> Vec<String> {
        let now = shown(app);
        let mut lines = Vec::new();
        match &self.shown {
            None => {
                let game = text::ascii(app.profile.profile().game.name);
                lines.push(format!("edbfi - {game} trainer"));
                lines.push(format!("State: {}", now.state));
                lines.push(file_line(
                    "Settings",
                    app.model.paths.settings.as_deref(),
                    "not saved this session",
                ));
                lines.push(file_line(
                    "Log",
                    app.model.paths.log.as_deref(),
                    "not written",
                ));
                let width = now
                    .rows
                    .iter()
                    .map(|(label, _)| label.len())
                    .max()
                    .unwrap_or(0);
                for (pane, items) in compact_panes(app) {
                    lines.push(format!("{pane}:"));
                    for row in items {
                        let label = text::ascii(app.label(row));
                        let mark = if app.marked(row) { "*" } else { " " };
                        let binding = app
                            .binding(row)
                            .and_then(|binding| binding.0)
                            .map(|chord| chord.to_string())
                            .unwrap_or_default();
                        lines.push(
                            format!(
                                "  {mark} {label:<width$}  {binding:<12}  {}",
                                app.status(row).word()
                            )
                            .trim_end()
                            .to_owned(),
                        );
                    }
                }
                lines.extend(now.card.iter().flatten().cloned());
                if let Some(status) = &now.status {
                    lines.push(status.clone());
                }
            }
            Some(before) => {
                if before.state != now.state {
                    lines.push(format!("State: {}", now.state));
                }
                if before.card != now.card {
                    lines.extend(now.card.iter().flatten().cloned());
                }
                for ((label, old), (_, new)) in before.rows.iter().zip(&now.rows) {
                    if old != new {
                        lines.push(format!("{label}: {new}"));
                    }
                }
                if now.status.is_some() && before.status != now.status {
                    lines.extend(now.status.clone());
                }
            }
        }
        self.shown = Some(now);
        lines
    }
}

fn file_line(name: &str, path: Option<&std::path::Path>, missing: &str) -> String {
    match path {
        Some(path) => format!("{name}: {}", text::ascii(&path.display().to_string())),
        None => format!("{name}: {missing}"),
    }
}

/// The compact tabs: every row once, grouped as on screen.
fn compact_panes(app: &App<'_>) -> Vec<(String, Vec<Row>)> {
    let mut panes: Vec<(String, Vec<Row>)> = app
        .groups
        .iter()
        .map(|group| {
            (
                text::ascii(group.title),
                group.toggles.iter().map(|id| Row::Toggle(*id)).collect(),
            )
        })
        .collect();
    let mut options = vec![Row::Master];
    options.extend(app.profile.options().map(|(id, _)| Row::Option(id)));
    options.extend([Row::Sounds, Row::StartPaused]);
    panes.push(("Options".into(), options));
    debug_assert!(
        app.panes()
            .iter()
            .flat_map(|(_, items)| items)
            .filter(|item| matches!(item, Item::Row(_)))
            .count()
            == panes.iter().map(|(_, rows)| rows.len()).sum::<usize>()
    );
    panes
}

fn shown(app: &App<'_>) -> Shown {
    let state = text::ascii(&crate::view::plain_state(app));
    let card = crate::view::plain_card(app)
        .map(|lines| lines.iter().map(|line| text::ascii(line)).collect());
    let rows = compact_panes(app)
        .into_iter()
        .flat_map(|(_, rows)| rows)
        .map(|row| {
            let mut status = app.status(row).word().to_owned();
            if let Some(chord) = app.binding(row).and_then(|binding| binding.0) {
                status = format!("{status} ({chord})");
            }
            (text::ascii(app.label(row)), status)
        })
        .collect();
    let status = app
        .model
        .notice
        .as_ref()
        .map(|notice| format!("Notice: {}", text::ascii(&notice.text)))
        .or_else(|| match &app.model.last {
            Some(Last::Message { text: message, .. }) => {
                Some(format!("Note: {}", text::ascii(message)))
            }
            _ => None,
        });
    Shown {
        state,
        card,
        rows,
        status,
    }
}
