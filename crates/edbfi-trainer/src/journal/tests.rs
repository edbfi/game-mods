// SPDX-License-Identifier: AGPL-3.0-or-later
#![forbid(unsafe_code)]

use std::fs;
use std::time::{Duration, SystemTime};

use super::*;
use crate::testdir::TestDir;

fn size(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |meta| meta.len())
}

fn lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_log_starts_with_a_header_and_holds_each_entry() {
    let dir = TestDir::new("journal-header");
    let mut journal = Journal::open(dir.path(), "metro-2033-redux/trainer 0.0.0");
    journal.record(Event::info(EntryKind::Settings, "saved"));
    let log = lines(&dir.path().join("trainer.log"));
    assert_eq!(log.len(), 2);
    assert!(
        log[0].starts_with("=== metro-2033-redux/trainer 0.0.0 started ")
            && log[0].ends_with(" UTC ==="),
        "{}",
        log[0]
    );
    assert!(
        log[1].starts_with('+') && log[1].ends_with("s info  settings: saved"),
        "{}",
        log[1]
    );
    assert_eq!(
        journal.path(),
        Some(dir.path().join("trainer.log").as_path())
    );
    assert!(journal.failure().is_none());
}

#[test]
fn an_entry_is_one_line_with_its_site_and_os_code() {
    let entry = Entry {
        at: Duration::from_millis(12_345),
        event: Event::warning(EntryKind::Settings, "could not save")
            .at_site(SiteId(3))
            .with_os_error(&io::Error::from_raw_os_error(5)),
    };
    assert_eq!(
        entry.to_string(),
        "+12.345s warn  settings: could not save (site S3) (OS error 5)"
    );
}

#[test]
fn activity_keeps_the_most_recent_entries() {
    let mut journal = Journal::memory();
    for n in 0..100 {
        journal.record(Event::info(EntryKind::Session, format!("entry {n}")));
    }
    let messages: Vec<&str> = journal
        .activity()
        .map(|e| e.event.message.as_str())
        .collect();
    assert_eq!(messages.len(), Journal::ACTIVITY);
    assert_eq!(messages[0], "entry 36");
    assert_eq!(messages[63], "entry 99");
    assert_eq!(journal.path(), None);
}

#[test]
fn messages_are_capped_and_cleaned() {
    let mut journal = Journal::memory();
    journal.record(Event::error(EntryKind::Session, "a\nb\r\u{1b}[2J\u{202E}c"));
    journal.record(Event::info(EntryKind::Session, "é".repeat(4000)));
    let mut activity = journal.activity();
    assert_eq!(
        activity.next().unwrap().event.message,
        r"a\u{a}b\u{d}\u{1b}[2J\u{202e}c"
    );
    let long = &activity.next().unwrap().event.message;
    assert!(long.len() <= Journal::MESSAGE_CAP + '…'.len_utf8());
    assert!(long.ends_with("é…"));
}

#[test]
fn the_log_rotates_before_passing_its_cap() {
    let dir = TestDir::new("journal-rotate");
    let (log, old) = (
        dir.path().join("trainer.log"),
        dir.path().join("trainer.old.log"),
    );
    let cap = 4096;
    let mut journal = Journal::with_cap(dir.path(), "test", cap);
    for n in 0..1000 {
        journal.record(Event::info(
            EntryKind::Session,
            format!("entry {n} {}", "x".repeat(n % 97)),
        ));
        assert!(size(&log) <= cap && size(&old) <= cap, "entry {n}");
    }
    assert!(journal.failure().is_none());
    let last = lines(&log);
    assert!(last.last().unwrap().contains("entry 999 "));
    let previous = lines(&old);
    let first_kept: usize = previous[0]
        .split_whitespace()
        .nth(4)
        .unwrap()
        .parse()
        .unwrap();
    let last_old: usize = previous
        .last()
        .unwrap()
        .split_whitespace()
        .nth(4)
        .unwrap()
        .parse()
        .unwrap();
    assert!(first_kept < last_old);
    assert!(last[0].contains(&format!("entry {} ", last_old + 1)));
}

#[test]
fn the_log_stays_within_one_mebibyte() {
    let dir = TestDir::new("journal-mib");
    let (log, old) = (
        dir.path().join("trainer.log"),
        dir.path().join("trainer.old.log"),
    );
    let mut journal = Journal::open(dir.path(), "test");
    let message = "y".repeat(Journal::MESSAGE_CAP);
    for _ in 0..2500 {
        journal.record(Event::info(EntryKind::Session, message.as_str()));
    }
    assert!(size(&old) > 0);
    assert!(size(&log) <= Journal::LOG_CAP && size(&old) <= Journal::LOG_CAP);
    assert!(Journal::LOG_CAP - size(&old) < 1100);
}

#[test]
fn a_log_that_cannot_open_keeps_activity_and_says_why() {
    let dir = TestDir::new("journal-open-fail");
    let file = dir.path().join("not-a-folder");
    fs::write(&file, "").unwrap();
    let mut journal = Journal::open(&file, "test");
    journal.record(Event::info(EntryKind::Session, "still here"));
    assert_eq!(journal.activity().len(), 1);
    let failure = journal.failure().unwrap();
    assert_eq!(failure.path, file.join("trainer.log"));
    assert!(failure.to_string().starts_with("the log "));
}

#[test]
fn a_failed_rotation_stops_the_log_and_keeps_activity() {
    let dir = TestDir::new("journal-rotate-fail");
    let old = dir.path().join("trainer.old.log");
    fs::create_dir(&old).unwrap();
    fs::write(old.join("blocker"), "").unwrap();
    let mut journal = Journal::with_cap(dir.path(), "test", 200);
    for n in 0..20 {
        journal.record(Event::info(EntryKind::Session, format!("entry {n}")));
    }
    assert!(journal.failure().is_some());
    assert_eq!(journal.activity().len(), 20);
    assert!(size(&dir.path().join("trainer.log")) <= 200);
}

#[test]
fn utc_dates_are_civil() {
    let at = |secs| utc(SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
    assert_eq!(at(0), "1970-01-01 00:00:00 UTC");
    assert_eq!(at(951_825_600), "2000-02-29 12:00:00 UTC");
    assert_eq!(at(1_793_575_084), "2026-11-01 23:18:04 UTC");
    assert_eq!(at(4_107_542_400), "2100-03-01 00:00:00 UTC");
    assert_eq!(
        utc(SystemTime::UNIX_EPOCH - Duration::from_secs(1)),
        "1970-01-01 00:00:00 UTC"
    );
}
