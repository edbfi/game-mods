// SPDX-License-Identifier: AGPL-3.0-only
//! The trainer's diagnostic record: the last [`Journal::ACTIVITY`] entries for the dashboard, and a log file
//! that never grows past [`Journal::LOG_CAP`].
//!
//! Each entry is formatted and written when it is recorded, on the caller's thread; there is no logging
//! thread, filter or global logger. When the log would pass its cap, `trainer.log` becomes `trainer.old.log`,
//! replacing the previous one, so the two files never exceed twice the cap. If the log cannot be written, the
//! activity list keeps working and [`Journal::failure`] reports why until the trainer exits.
#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::fmt::{self, Write as _};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::profile::SiteId;

const LOG_NAME: &str = "trainer.log";
const OLD_NAME: &str = "trainer.old.log";

/// How serious an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Something happened as expected.
    Info,
    /// Something went wrong and the trainer carried on.
    Warning,
    /// Something failed.
    Error,
}

impl Severity {
    const fn word(self) -> &'static str {
        match self {
            Self::Info => "info ",
            Self::Warning => "warn ",
            Self::Error => "error",
        }
    }
}

/// What part of the trainer an entry is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EntryKind {
    /// The trainer starting or stopping.
    Session,
    /// The settings file.
    Settings,
}

impl EntryKind {
    const fn word(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Settings => "settings",
        }
    }
}

/// Something to record. The journal adds the time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// How serious it is.
    pub severity: Severity,
    /// What it is about.
    pub kind: EntryKind,
    /// One line for a person; at most [`Journal::MESSAGE_CAP`] bytes are kept, then an ellipsis.
    pub message: String,
    /// The hook site involved, if any.
    pub site: Option<SiteId>,
    /// The operating system's error code, if any.
    pub os_code: Option<i32>,
}

impl Event {
    /// An [`Severity::Info`] event.
    #[must_use]
    pub fn info(kind: EntryKind, message: impl Into<String>) -> Self {
        Self::new(Severity::Info, kind, message.into())
    }

    /// A [`Severity::Warning`] event.
    #[must_use]
    pub fn warning(kind: EntryKind, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, kind, message.into())
    }

    /// An [`Severity::Error`] event.
    #[must_use]
    pub fn error(kind: EntryKind, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, kind, message.into())
    }

    const fn new(severity: Severity, kind: EntryKind, message: String) -> Self {
        Self {
            severity,
            kind,
            message,
            site: None,
            os_code: None,
        }
    }

    /// This event, about hook site `site`.
    #[must_use]
    pub const fn at_site(mut self, site: SiteId) -> Self {
        self.site = Some(site);
        self
    }

    /// This event, with the operating system's error code from `error` if it has one.
    #[must_use]
    pub fn with_os_error(mut self, error: &io::Error) -> Self {
        self.os_code = error.raw_os_error();
        self
    }
}

/// A recorded [`Event`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Time since the journal started.
    pub at: Duration,
    /// The event, its message capped and stripped of control characters.
    pub event: Event,
}

impl fmt::Display for Entry {
    /// One log line, without the newline.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Event {
            severity,
            kind,
            message,
            site,
            os_code,
        } = &self.event;
        write!(
            f,
            "+{}.{:03}s {} {}: {message}",
            self.at.as_secs(),
            self.at.subsec_millis(),
            severity.word(),
            kind.word()
        )?;
        if let Some(site) = site {
            write!(f, " (site {site})")?;
        }
        if let Some(code) = os_code {
            write!(f, " (OS error {code})")?;
        }
        Ok(())
    }
}

/// Why the log file stopped being written.
#[derive(Debug)]
pub struct SinkFailure {
    /// The log file.
    pub path: PathBuf,
    /// What failed.
    pub error: io::Error,
}

impl fmt::Display for SinkFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the log {} is not being written: {}",
            self.path.display(),
            self.error
        )
    }
}

/// The trainer's diagnostic record.
#[derive(Debug)]
pub struct Journal {
    started: Instant,
    activity: VecDeque<Entry>,
    path: Option<PathBuf>,
    sink: Option<Sink>,
    failure: Option<SinkFailure>,
}

impl Journal {
    /// How many entries the activity list keeps.
    pub const ACTIVITY: usize = 64;
    /// The most bytes `trainer.log` grows to before it is rotated.
    pub const LOG_CAP: u64 = 1024 * 1024;
    /// The most bytes of an event's message that are kept; a longer one also gets an ellipsis.
    pub const MESSAGE_CAP: usize = 1024;

    /// A journal writing `trainer.log` in `dir`, which is created if needed. The log starts with `header`
    /// and the current UTC time. If the log cannot be opened the journal still keeps activity, and
    /// [`Journal::failure`] says why.
    #[must_use]
    pub fn open(dir: &Path, header: &str) -> Self {
        Self::with_cap(dir, header, Self::LOG_CAP)
    }

    fn with_cap(dir: &Path, header: &str, cap: u64) -> Self {
        let path = dir.join(LOG_NAME);
        let mut journal = Self {
            path: Some(path.clone()),
            ..Self::memory()
        };
        let opened = fs::create_dir_all(dir).and_then(|()| {
            Ok(Sink {
                file: append(&path)?,
                old: dir.join(OLD_NAME),
                path: path.clone(),
                cap,
            })
        });
        match opened {
            Ok(sink) => journal.sink = Some(sink),
            Err(error) => journal.failure = Some(SinkFailure { path, error }),
        }
        let started = utc(SystemTime::now());
        journal.write(&format!("=== {} started {started} ===", clean(header)));
        journal
    }

    /// A journal with no log file, keeping only activity.
    #[must_use]
    pub fn memory() -> Self {
        Self {
            started: Instant::now(),
            activity: VecDeque::with_capacity(Self::ACTIVITY),
            path: None,
            sink: None,
            failure: None,
        }
    }

    /// Records `event`: adds it to the activity list and appends it to the log.
    pub fn record(&mut self, mut event: Event) {
        event.message = clean(&event.message);
        let entry = Entry {
            at: self.started.elapsed(),
            event,
        };
        self.write(&entry.to_string());
        if self.activity.len() == Self::ACTIVITY {
            self.activity.pop_front();
        }
        self.activity.push_back(entry);
    }

    /// The most recent entries, oldest first.
    pub fn activity(&self) -> impl ExactSizeIterator<Item = &Entry> {
        self.activity.iter()
    }

    /// The log file, if this journal has one.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Why the log file is not being written, if it is not.
    #[must_use]
    pub const fn failure(&self) -> Option<&SinkFailure> {
        self.failure.as_ref()
    }

    fn write(&mut self, line: &str) {
        let Some(sink) = &mut self.sink else {
            return;
        };
        if let Err(error) = sink.append(line) {
            let path = sink.path.clone();
            self.sink = None;
            self.failure = Some(SinkFailure { path, error });
        }
    }
}

/// The open log file.
#[derive(Debug)]
struct Sink {
    file: File,
    path: PathBuf,
    old: PathBuf,
    cap: u64,
}

impl Sink {
    /// Appends `line` and a newline, first rotating the log if the line would take it past the cap.
    fn append(&mut self, line: &str) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        let len = self.file.metadata()?.len();
        if len > 0 && len.saturating_add(bytes.len() as u64) > self.cap {
            // The standard library opens files with delete sharing, so Windows can rename the open log.
            fs::rename(&self.path, &self.old)?;
            self.file = append(&self.path)?;
        }
        self.file.write_all(&bytes)
    }
}

fn append(path: &Path) -> io::Result<File> {
    OpenOptions::new().append(true).create(true).open(path)
}

/// `text` capped to [`Journal::MESSAGE_CAP`] bytes, with control and bidirectional formatting characters
/// written as escapes, so a log line stays one line and reads as written.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(Journal::MESSAGE_CAP));
    for c in text.chars() {
        let before = out.len();
        if c.is_control() || is_bidi_control(c) {
            let _infallible = write!(out, "{}", c.escape_unicode());
        } else {
            out.push(c);
        }
        if out.len() > Journal::MESSAGE_CAP {
            out.truncate(before);
            out.push('…');
            break;
        }
    }
    out
}

const fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// `time` as `YYYY-MM-DD HH:MM:SS UTC`, or the epoch if it is before 1970.
fn utc(time: SystemTime) -> String {
    let secs = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Days to a civil date, after Howard Hinnant's `civil_from_days`, for non-negative day counts.
    let z = days + 719_468;
    let (era, doe) = (z / 146_097, z % 146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests;
