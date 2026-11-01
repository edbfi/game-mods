// SPDX-License-Identifier: AGPL-3.0-only
//! Strict reading of the settings file, with the line and column of every problem.
#![forbid(unsafe_code)]

use std::fmt;
use std::ops::Range;

use toml::de::{DeTable, DeValue};

use crate::chord::{Binding, Chord, ChordError, Refusal};
use crate::profile::ValidProfile;
use crate::recovery::RecoveryAction;

use super::{Field, MAX_BYTES, Parsed, Settings, Slot, Target, Upgrade, fields};

const BOM: &[u8] = "\u{FEFF}".as_bytes();

/// A settings file the trainer cannot use.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{}{problem}", at(*location))]
pub struct SettingsError {
    /// Where the problem is, if it has a place in the file.
    pub location: Option<Location>,
    /// What is wrong.
    pub problem: Problem,
}

fn at(location: Option<Location>) -> String {
    location.map(|l| format!("{l}: ")).unwrap_or_default()
}

impl SettingsError {
    /// Fix the file, or reset it if this trainer cannot read its schema at all.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        match self.problem {
            Problem::NewerSchema { .. } => RecoveryAction::ResetSettings,
            _ => RecoveryAction::EditSettings,
        }
    }
}

/// A position in the file: 1-based line, and 1-based column counted in characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Location {
    /// The line.
    pub line: u32,
    /// The column.
    pub column: u32,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}, column {}", self.line, self.column)
    }
}

/// What is wrong with a settings file. Names from the file are quoted with escapes, so they cannot carry
/// terminal control sequences.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Problem {
    /// Larger than [`MAX_BYTES`].
    #[error("the file is larger than {MAX_BYTES} bytes")]
    TooLarge,
    /// Not UTF-8 text.
    #[error("the file is not UTF-8 text")]
    NotUtf8,
    /// Two byte order marks; editors add at most one.
    #[error("the file starts with two byte order marks")]
    SecondBom,
    /// Not valid TOML.
    #[error("{message}")]
    Syntax {
        /// The TOML parser's explanation.
        message: String,
    },
    /// No top-level `schema` key.
    #[error("the file has no `schema = {supported}` line")]
    MissingSchema {
        /// The schema this trainer writes.
        supported: u32,
    },
    /// A `schema` that is not a whole number from 1.
    #[error("schema must be a whole number from 1 to {supported}")]
    BadSchema {
        /// The schema this trainer writes.
        supported: u32,
    },
    /// A schema newer than this trainer.
    #[error("schema {found} is newer than this trainer's {supported}; update the trainer or reset")]
    NewerSchema {
        /// The file's schema.
        found: u32,
        /// The schema this trainer writes.
        supported: u32,
    },
    /// A table this schema does not have.
    #[error("unknown table [{}]", quoted(table))]
    UnknownTable {
        /// The table's name.
        table: String,
    },
    /// A table of this schema is missing.
    #[error("missing table [{table}]")]
    MissingTable {
        /// The table's name.
        table: String,
    },
    /// A key that should be a table is something else.
    #[error("{table} must be a table, written [{table}]")]
    NotATable {
        /// The table's name.
        table: String,
    },
    /// A key this schema does not have.
    #[error("unknown key {} in {}", quoted(key), place(table))]
    UnknownKey {
        /// The table, empty for the top level.
        table: String,
        /// The key.
        key: String,
    },
    /// A key of this schema is missing.
    #[error("missing key {key} in [{table}]")]
    MissingKey {
        /// The table.
        table: String,
        /// The key.
        key: String,
    },
    /// A value of the wrong type.
    #[error("{key} in [{table}] must be {expected}")]
    WrongType {
        /// The table.
        table: String,
        /// The key.
        key: String,
        /// What it must be.
        expected: Expected,
    },
    /// A binding that is not a chord or `None`.
    #[error("{key} in [bindings]: {error}")]
    BadBinding {
        /// The binding's key.
        key: String,
        /// Why the text is not a chord.
        error: ChordError,
    },
    /// A binding the policy refuses.
    #[error("{key} in [bindings] cannot be {chord}: {refusal}")]
    RefusedBinding {
        /// The binding's key.
        key: String,
        /// The refused chord, canonically spelled.
        chord: String,
        /// Why.
        refusal: Refusal,
    },
    /// A chord bound twice.
    #[error("{key} and {first} in [bindings] are both {chord}; change or set one to \"None\"")]
    DuplicateBinding {
        /// The later binding's key.
        key: String,
        /// The earlier binding's key.
        first: String,
        /// The chord, canonically spelled.
        chord: String,
    },
}

/// The type a key must have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expected {
    /// `true` or `false`.
    Bool,
    /// A quoted binding such as `"Alt+F1"` or `"None"`.
    Binding,
}

impl fmt::Display for Expected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bool => "true or false",
            Self::Binding => "a quoted binding such as \"Alt+F1\" or \"None\"",
        })
    }
}

/// `text` as a quoted, escaped string.
fn quoted(text: &str) -> String {
    format!("{text:?}")
}

fn place(table: &str) -> String {
    if table.is_empty() {
        "the top level".to_owned()
    } else {
        format!("[{}]", quoted(table))
    }
}

/// The 1-based line and character column of byte `offset` in `text`.
fn locate(text: &str, offset: usize) -> Location {
    let before = text.get(..offset).unwrap_or(text);
    let line_start = before.rfind('\n').map_or(0, |at| at + 1);
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX).saturating_add(1);
    Location {
        line: count(before.matches('\n').count()),
        column: count(before[line_start..].chars().count()),
    }
}

/// Collects problems and keeps the earliest in the file.
struct Problems<'t> {
    text: &'t str,
    found: Vec<SettingsError>,
}

impl Problems<'_> {
    fn add(&mut self, span: Option<Range<usize>>, problem: Problem) {
        let location = span.map(|span| locate(self.text, span.start));
        self.found.push(SettingsError { location, problem });
    }

    fn first(self) -> Option<SettingsError> {
        self.found
            .into_iter()
            .min_by_key(|error| (error.location.is_none(), error.location))
    }
}

pub(super) fn parse(profile: &ValidProfile<'_>, bytes: &[u8]) -> Result<Parsed, SettingsError> {
    let fail = |location, problem| SettingsError { location, problem };
    if bytes.len() > MAX_BYTES {
        return Err(fail(None, Problem::TooLarge));
    }
    let bytes = bytes.strip_prefix(BOM).unwrap_or(bytes);
    // The TOML parser would skip a second one.
    if bytes.starts_with(BOM) {
        return Err(fail(Some(locate("", 0)), Problem::SecondBom));
    }
    let text = std::str::from_utf8(bytes).map_err(|error| {
        let valid = &bytes[..error.valid_up_to()];
        let valid = std::str::from_utf8(valid).unwrap_or_default();
        fail(Some(locate(valid, valid.len())), Problem::NotUtf8)
    })?;
    let root = DeTable::parse(text)
        .map_err(|error| {
            fail(
                error.span().map(|span| locate(text, span.start)),
                Problem::Syntax {
                    message: error.message().trim_end().to_owned(),
                },
            )
        })?
        .into_inner();
    let schema = schema(profile, text, &root)?;

    let fields: Vec<Field> = fields(profile)
        .into_iter()
        .filter(|field| field.since <= schema)
        .collect();
    let mut problems = Problems {
        text,
        found: Vec::new(),
    };
    let mut settings = Settings::defaults(profile);
    let mut bound: Vec<(usize, Target, Chord)> = Vec::new();

    for (key, value) in &root {
        let name = key.get_ref().as_ref();
        if name == "schema" {
            continue;
        }
        let is_table = matches!(value.get_ref(), DeValue::Table(_));
        if !fields.iter().any(|field| field.table == name) {
            let problem = if is_table {
                Problem::UnknownTable {
                    table: name.to_owned(),
                }
            } else {
                Problem::UnknownKey {
                    table: String::new(),
                    key: name.to_owned(),
                }
            };
            problems.add(Some(key.span()), problem);
        } else if !is_table {
            let table = name.to_owned();
            problems.add(Some(key.span()), Problem::NotATable { table });
        }
    }

    let mut tables: Vec<&'static str> = fields.iter().map(|field| field.table).collect();
    tables.dedup();
    for table in tables {
        let Some((header, value)) = root.get_key_value(table) else {
            let table = table.to_owned();
            problems.add(None, Problem::MissingTable { table });
            continue;
        };
        let DeValue::Table(entries) = value.get_ref() else {
            continue;
        };
        let expected: Vec<&Field> = fields.iter().filter(|f| f.table == table).collect();
        for (key, value) in entries {
            let name = key.get_ref().as_ref();
            let Some(field) = expected.iter().find(|field| field.key == name) else {
                let problem = Problem::UnknownKey {
                    table: table.to_owned(),
                    key: name.to_owned(),
                };
                problems.add(Some(key.span()), problem);
                continue;
            };
            let wrong_type = |expected| Problem::WrongType {
                table: table.to_owned(),
                key: field.key.to_owned(),
                expected,
            };
            match (field.slot, value.get_ref()) {
                (Slot::Binding(target), DeValue::String(text)) => match binding(field.key, text) {
                    Ok(found) => {
                        settings.bindings.set(target, found);
                        if let Some(chord) = found.0 {
                            bound.push((key.span().start, target, chord));
                        }
                    }
                    Err(problem) => problems.add(Some(value.span()), problem),
                },
                (Slot::Binding(_), _) => {
                    problems.add(Some(value.span()), wrong_type(Expected::Binding))
                }
                (slot, DeValue::Boolean(on)) => match slot {
                    Slot::StartPaused => settings.start_paused = *on,
                    Slot::Sounds => settings.sounds = *on,
                    Slot::Toggle(id) => settings.selection.set(id, *on),
                    Slot::Option(id) => settings.options.set(id, *on),
                    Slot::Binding(_) => {}
                },
                _ => problems.add(Some(value.span()), wrong_type(Expected::Bool)),
            }
        }
        for field in &expected {
            if !entries.contains_key(field.key) {
                let problem = Problem::MissingKey {
                    table: table.to_owned(),
                    key: field.key.to_owned(),
                };
                problems.add(Some(header.span()), problem);
            }
        }
    }

    bound.sort_by_key(|(offset, _, _)| *offset);
    for (i, (offset, target, chord)) in bound.iter().enumerate() {
        if let Some((_, first, _)) = bound[..i].iter().find(|(_, _, c)| c == chord) {
            let problem = Problem::DuplicateBinding {
                key: target.key(profile).to_owned(),
                first: first.key(profile).to_owned(),
                chord: chord.to_string(),
            };
            problems.add(Some(*offset..*offset), problem);
        }
    }

    if let Some(error) = problems.first() {
        return Err(error);
    }
    let upgrade = (schema < profile.profile().settings_schema).then(|| Upgrade {
        from: schema,
        unbound: unbind_taken_defaults(profile, &mut settings, schema),
    });
    Ok(Parsed { settings, upgrade })
}

/// The file's schema, checked to lie in `1..=settings_schema`.
fn schema(
    profile: &ValidProfile<'_>,
    text: &str,
    root: &DeTable<'_>,
) -> Result<u32, SettingsError> {
    let supported = profile.profile().settings_schema;
    let Some(value) = root.get("schema") else {
        return Err(SettingsError {
            location: None,
            problem: Problem::MissingSchema { supported },
        });
    };
    let location = Some(locate(text, value.span().start));
    let found = match value.get_ref() {
        DeValue::Integer(n) => u32::from_str_radix(n.as_str(), n.radix()).ok(),
        _ => None,
    };
    match found {
        Some(found) if (1..=supported).contains(&found) => Ok(found),
        Some(found) if found > supported => Err(SettingsError {
            location,
            problem: Problem::NewerSchema { found, supported },
        }),
        _ => Err(SettingsError {
            location,
            problem: Problem::BadSchema { supported },
        }),
    }
}

/// Parses one binding value and applies the policy.
fn binding(key: &str, text: &str) -> Result<Binding, Problem> {
    let found: Binding = text.parse().map_err(|error| Problem::BadBinding {
        key: key.to_owned(),
        error,
    })?;
    if let Some(chord) = found.0 {
        chord
            .check_policy()
            .map_err(|refusal| Problem::RefusedBinding {
                key: key.to_owned(),
                chord: chord.to_string(),
                refusal,
            })?;
    }
    Ok(found)
}

/// Leaves unbound every binding newer than `schema` whose default chord an older binding already uses, in
/// file order. Returns the targets left unbound.
fn unbind_taken_defaults(
    profile: &ValidProfile<'_>,
    settings: &mut Settings,
    schema: u32,
) -> Vec<Target> {
    let mut unbound = Vec::new();
    for field in fields(profile) {
        let Slot::Binding(target) = field.slot else {
            continue;
        };
        if field.since <= schema {
            continue;
        }
        let Some(chord) = settings.bindings.get(target).0 else {
            continue;
        };
        if settings.bindings.holder_other_than(chord, target).is_some() {
            settings.bindings.set(target, Binding(None));
            unbound.push(target);
        }
    }
    unbound
}
