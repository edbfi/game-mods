// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]

use std::num::NonZeroU32;
use std::panic::{AssertUnwindSafe, catch_unwind};

use super::fake::{Fake, Fault, PAGE};
use super::freeze::{FreezeKind, ResumeError};
use super::*;

const CODE: u64 = 0x1000_0000;
const READ_ONLY: Protection = Protection::new(0x02);

/// Three code pages, the middle one read-only so a patch crossing into it spans two regions, and two
/// threads parked well away from the patches.
fn game() -> Fake {
    let fake = Fake::new(77);
    fake.map(CODE, PAGE, Protection::EXECUTE_READ, 0xCC);
    fake.map(CODE + PAGE, PAGE, READ_ONLY, 0xCC);
    fake.map(CODE + 2 * PAGE, PAGE, Protection::EXECUTE_READ, 0xCC);
    for (site, at, original) in originals() {
        let _ = site;
        fake.poke(at, original);
    }
    fake.thread(1, CODE + 0x800);
    fake.thread(2, CODE + 2 * PAGE + 0x800);
    fake
}

fn originals() -> [(SiteId, u64, &'static [u8]); 3] {
    [
        (SiteId(1), CODE + 0x100, &[0x48, 0x89, 0x5C, 0x24, 0x08]),
        (
            SiteId(2),
            CODE + PAGE - 2,
            &[0x49, 0x8B, 0x06, 0x41, 0x2B, 0xFD],
        ),
        (
            SiteId(3),
            CODE + 2 * PAGE + 0x40,
            &[0x40, 0x53, 0x48, 0x83, 0xEC],
        ),
    ]
}

fn range(at: u64, len: usize) -> CodeRange {
    CodeRange::new(Va::new(at), NonZeroU32::new(len as u32).unwrap()).unwrap()
}

fn hooks() -> PatchJournal {
    PatchJournal::new(
        originals()
            .into_iter()
            .map(|(site, at, original)| Patch {
                site,
                range: range(at, original.len()),
                original: original.to_vec(),
                replacement: std::iter::once(0xE9)
                    .chain((1..original.len()).map(|i| 0x10 + i as u8))
                    .collect(),
            })
            .collect(),
    )
    .unwrap()
}

fn ranges(journal: &PatchJournal) -> Vec<CodeRange> {
    journal.ranges().collect()
}

/// Every site holds its original bytes and every page its first protection.
fn pristine(fake: &Fake) -> bool {
    originals()
        .iter()
        .all(|&(_, at, original)| fake.peek(at, original.len()) == original)
        && fake.protection(CODE) == Protection::EXECUTE_READ
        && fake.protection(CODE + PAGE) == READ_ONLY
        && fake.protection(CODE + 2 * PAGE) == Protection::EXECUTE_READ
}

fn installed(fake: &Fake, journal: &PatchJournal) -> bool {
    journal
        .patches
        .iter()
        .all(|patch| fake.peek(patch.at().get(), patch.replacement.len()) == patch.replacement)
}

/// Every site's bytes are each its original or replacement byte.
fn only_ours(fake: &Fake, journal: &PatchJournal) -> bool {
    journal
        .patches
        .iter()
        .all(|patch| ours(&fake.peek(patch.at().get(), patch.original.len()), patch))
}

fn apply_once(fake: &Fake, journal: &mut PatchJournal) -> Result<(), ApplyError> {
    let ranges = ranges(journal);
    let frozen = Freeze::new(fake, &ranges).unwrap();
    let result = apply(&frozen, journal);
    frozen.resume().unwrap();
    result
}

fn restore_once(fake: &Fake, journal: &mut PatchJournal) -> Result<(), RestoreIncomplete> {
    let ranges = ranges(journal);
    let frozen = Freeze::new(fake, &ranges).unwrap();
    let result = restore(&frozen, journal);
    frozen.resume().unwrap();
    result
}

#[test]
fn apply_then_restore_returns_bytes_protections_and_threads() {
    let fake = game();
    let mut journal = hooks();
    apply_once(&fake, &mut journal).unwrap();
    assert!(installed(&fake, &journal));
    assert_eq!(journal.owned(), [SiteId(1), SiteId(2), SiteId(3)]);
    assert_eq!(fake.protection(CODE + PAGE), READ_ONLY);
    assert_eq!(fake.protection(CODE), Protection::EXECUTE_READ);
    assert_eq!(fake.suspend_counts(), [0, 0]);
    restore_once(&fake, &mut journal).unwrap();
    assert!(pristine(&fake));
    assert!(journal.is_clean());
    assert_eq!(fake.suspend_counts(), [0, 0]);
}

#[test]
fn a_changed_original_refuses_before_any_write() {
    let fake = game();
    fake.poke(CODE + 2 * PAGE + 0x40, &[0x90]);
    let mut journal = hooks();
    let error = apply_once(&fake, &mut journal).unwrap_err();
    assert_eq!(error.cause, PatchError::Conflict { site: SiteId(3) });
    assert_eq!(error.cause.recovery(), RecoveryAction::ModifiedGameCode);
    assert_eq!(fake.calls("write"), 0);
    assert!(journal.is_clean());
}

#[test]
fn installed_hooks_refuse_a_second_apply() {
    let fake = game();
    let mut journal = hooks();
    apply_once(&fake, &mut journal).unwrap();
    let error = apply_once(&fake, &mut journal).unwrap_err();
    assert_eq!(error.cause, PatchError::AlreadyOwned);
}

#[test]
fn journals_refuse_overlapping_or_misfit_patches() {
    let patch = |site, at, len: usize| Patch {
        site: SiteId(site),
        range: range(at, len),
        original: vec![0; len],
        replacement: vec![1; len],
    };
    assert!(PatchJournal::new(vec![patch(1, 0x100, 5), patch(2, 0x105, 5)]).is_ok());
    assert_eq!(
        PatchJournal::new(vec![patch(1, 0x100, 5), patch(2, 0x104, 5)]).unwrap_err(),
        PatchError::Invalid { site: SiteId(2) }
    );
    let mut short = patch(3, 0x200, 5);
    short.replacement.pop();
    assert_eq!(
        PatchJournal::new(vec![short]).unwrap_err(),
        PatchError::Invalid { site: SiteId(3) }
    );
}

/// The number of calls a clean install and restore make.
fn clean_calls() -> usize {
    let fake = game();
    let mut journal = hooks();
    apply_once(&fake, &mut journal).unwrap();
    restore_once(&fake, &mut journal).unwrap();
    fake.total_calls()
}

/// Runs install then restore with `fault` planned on the `nth` call to `call`, then restores again without
/// faults. Whatever failed, the game ends exactly as it began and the journal clean.
fn survives(call: &'static str, nth: usize, fault: Fault) {
    let fake = game();
    fake.fault(call, nth, fault);
    let mut journal = hooks();
    let context = format!("{fault:?} on {call} #{nth}");
    let applied = catch_unwind(AssertUnwindSafe(|| apply_once(&fake, &mut journal)));
    match &applied {
        Ok(Ok(())) => assert!(installed(&fake, &journal), "{context}"),
        Ok(Err(error)) => {
            // A failed install either rolled back completely or says what it left.
            match &error.rollback {
                Ok(()) => assert!(journal.is_clean() && pristine(&fake), "{context}: {error}"),
                Err(left) => assert!(
                    !journal.is_clean() && left.debt == journal.debt(),
                    "{context}"
                ),
            }
        }
        Err(_) => {}
    }
    assert!(only_ours(&fake, &journal), "{context}");
    assert_eq!(fake.suspend_counts(), [0, 0], "{context}");
    let restored = catch_unwind(AssertUnwindSafe(|| restore_once(&fake, &mut journal)));
    if let Ok(Err(left)) = &restored {
        assert_eq!(left.debt, journal.debt(), "{context}");
        assert_eq!(
            left.sites
                .iter()
                .map(|failure| failure.site)
                .collect::<Vec<_>>(),
            journal.owned(),
            "{context}"
        );
    }
    fake.clear_faults();
    restore_once(&fake, &mut journal).unwrap_or_else(|left| panic!("{context}: {left}"));
    assert!(journal.is_clean() && pristine(&fake), "{context}");
    assert_eq!(fake.suspend_counts(), [0, 0], "{context}");
}

#[test]
fn every_failed_call_is_rolled_back_or_reported() {
    let calls = clean_calls();
    for call in ["read", "query", "protect", "write", "flush"] {
        for nth in 0..calls {
            survives(call, nth, Fault::Fail);
        }
    }
}

#[test]
fn every_short_write_is_owned_and_restored() {
    for nth in 0..8 {
        for written in 0..6 {
            survives("write", nth, Fault::Partial(written));
        }
    }
}

#[test]
fn a_panic_at_any_call_leaves_ownership_recorded() {
    let calls = clean_calls();
    for call in ["read", "query", "protect", "write", "flush"] {
        for nth in 0..calls {
            survives(call, nth, Fault::Panic);
        }
    }
}

#[test]
fn foreign_bytes_are_kept_and_the_rest_restored() {
    let fake = game();
    let mut journal = hooks();
    apply_once(&fake, &mut journal).unwrap();
    fake.poke(CODE + PAGE, &[0x77]);
    let left = restore_once(&fake, &mut journal).unwrap_err();
    assert_eq!(
        left.sites,
        [SiteFailure {
            site: SiteId(2),
            fault: SiteFault::Foreign
        }]
    );
    assert_eq!(left.recovery(), RecoveryAction::RestartGame);
    assert_eq!(journal.owned(), [SiteId(2)]);
    assert_eq!(fake.peek(CODE + PAGE, 1), [0x77]);
    for (site, at, original) in originals() {
        if site != SiteId(2) {
            assert_eq!(fake.peek(at, original.len()), original);
        }
    }
    // Once the foreign byte is gone the site restores.
    fake.poke(CODE + PAGE, &[0x12]);
    restore_once(&fake, &mut journal).unwrap();
    assert!(journal.is_clean() && pristine(&fake));
}

#[test]
fn a_half_restored_site_still_counts_as_ours() {
    let fake = game();
    let mut journal = hooks();
    apply_once(&fake, &mut journal).unwrap();
    // The first two bytes went back; the rest are still the jump.
    fake.poke(CODE + 0x100, &[0x48, 0x89]);
    restore_once(&fake, &mut journal).unwrap();
    assert!(pristine(&fake));
}

#[test]
fn protection_debt_blocks_writes_until_settled() {
    let fake = game();
    let mut journal = hooks();
    // The second protect call is the first put-back of site 1's write.
    fake.fault("protect", 1, Fault::Fail);
    let error = apply_once(&fake, &mut journal).unwrap_err();
    assert!(matches!(
        error.cause,
        PatchError::Memory {
            site: SiteId(1),
            source: MemoryError::Reprotect { .. }
        }
    ));
    // The rollback settled the debt before writing the original back.
    error.rollback.unwrap();
    assert!(journal.is_clean() && pristine(&fake));
    // Debt that cannot be settled refuses the next write outright.
    let fake = game();
    let mut journal = hooks();
    for nth in 1..64 {
        fake.fault("protect", nth, Fault::Fail);
    }
    let error = apply_once(&fake, &mut journal).unwrap_err();
    assert_eq!(fake.calls("write"), 1);
    assert_eq!(error.rollback.as_ref().unwrap_err().debt, 1);
    assert_eq!(journal.debt(), 1);
    assert_eq!(fake.protection(CODE), Protection::EXECUTE_READ_WRITE);
    fake.clear_faults();
    restore_once(&fake, &mut journal).unwrap();
    assert!(journal.is_clean() && pristine(&fake));
}

#[test]
fn writes_need_committed_accessible_memory() {
    let fake = game();
    fake.map(CODE + 3 * PAGE, PAGE, Protection::new(0x120), 0);
    let patch = |at: u64| Patch {
        site: SiteId(9),
        range: range(at, 5),
        original: vec![0; 5],
        replacement: vec![1; 5],
    };
    for at in [CODE + 3 * PAGE, CODE + 5 * PAGE] {
        let mut journal = PatchJournal::new(vec![patch(at)]).unwrap();
        let frozen = Freeze::new(&fake, &[]).unwrap();
        assert_eq!(
            write_code(&frozen, &mut journal, Va::new(at), &[1; 5]),
            Err(MemoryError::Inaccessible { at: Va::new(at) })
        );
        assert_eq!(journal.debt(), 0);
    }
    assert_eq!(fake.calls("write"), 0);
}

#[test]
fn a_freeze_suspends_each_thread_once_and_keeps_earlier_suspensions() {
    let fake = game();
    fake.0.borrow_mut().threads.get_mut(&2).unwrap().suspended = 1;
    let frozen = Freeze::new(&fake, &[]).unwrap();
    assert_eq!(frozen.threads(), 2);
    assert_eq!(fake.suspend_counts(), [1, 2]);
    assert_eq!(fake.calls("threads"), 3);
    frozen.resume().unwrap();
    assert_eq!(fake.suspend_counts(), [0, 1]);
}

#[test]
fn a_freeze_waits_for_the_thread_set_to_settle() {
    let fake = game();
    fake.thread(3, CODE + 0x900);
    fake.0.borrow_mut().snapshots = [vec![1], vec![1, 2], vec![2, 1], vec![1, 2, 3]].into();
    let frozen = Freeze::new(&fake, &[]).unwrap();
    assert_eq!(frozen.threads(), 3);
    // Two more passes after the last change.
    assert_eq!(fake.calls("threads"), 6);
    drop(frozen);
    assert_eq!(fake.suspend_counts(), [0, 0, 0]);

    let fake = game();
    fake.0.borrow_mut().snapshots = (0..Freeze::PASSES)
        .map(|pass| if pass % 2 == 0 { vec![1] } else { vec![1, 2] })
        .collect();
    let error = Freeze::new(&fake, &[]).unwrap_err();
    assert_eq!(error.kind, FreezeKind::Unstable);
    assert!(error.transient());
    assert_eq!(fake.suspend_counts(), [0, 0]);
}

#[test]
fn a_failed_freeze_resumes_what_it_suspended() {
    let cases: [(&str, usize, FreezeKind); 5] = [
        (
            "threads",
            0,
            FreezeKind::Enumerate(OsError::new("Thread32Next", 5)),
        ),
        (
            "open_thread",
            1,
            FreezeKind::Open {
                thread: 2,
                source: OsError::new("OpenThread", 5),
            },
        ),
        (
            "thread_owner",
            1,
            FreezeKind::Open {
                thread: 2,
                source: OsError::new("thread_owner", 5),
            },
        ),
        (
            "suspend",
            1,
            FreezeKind::Suspend {
                thread: 2,
                source: OsError::new("suspend", 5),
            },
        ),
        (
            "context",
            1,
            FreezeKind::Context(OsError::new("context", 5)),
        ),
    ];
    for (call, nth, kind) in cases {
        let fake = game();
        fake.fault(call, nth, Fault::Fail);
        let error = Freeze::new(&fake, &[]).unwrap_err();
        assert_eq!(error.kind, kind, "{call}");
        assert_eq!(error.unresumed, 0);
        assert_eq!(fake.suspend_counts(), [0, 0], "{call}");
    }
    let fake = game();
    fake.0.borrow_mut().snapshots = [vec![]].into();
    assert_eq!(
        Freeze::new(&fake, &[]).unwrap_err().kind,
        FreezeKind::NoThreads
    );
    let fake = game();
    fake.0.borrow_mut().threads.get_mut(&2).unwrap().owner = 5;
    assert_eq!(
        Freeze::new(&fake, &[]).unwrap_err().kind,
        FreezeKind::Owner { thread: 2 }
    );
    assert_eq!(fake.suspend_counts(), [0, 0]);
}

#[test]
fn a_thread_inside_a_changing_range_refuses_the_freeze() {
    let site = range(CODE + 0x100, 5);
    for (rip, busy) in [
        (CODE + 0xFF, false),
        (CODE + 0x100, true),
        (CODE + 0x104, true),
        (CODE + 0x105, false),
    ] {
        let fake = game();
        fake.0.borrow_mut().threads.get_mut(&2).unwrap().rip = Va::new(rip);
        let result = Freeze::new(&fake, &[site]);
        assert_eq!(result.is_err(), busy, "{rip:#x}");
        match result {
            Ok(frozen) => frozen.resume().unwrap(),
            Err(error) => {
                assert_eq!(
                    error.kind,
                    FreezeKind::Busy {
                        rip: Va::new(rip),
                        range: site
                    }
                );
                assert!(error.transient());
            }
        }
        assert_eq!(fake.suspend_counts(), [0, 0]);
    }
}

#[test]
fn threads_that_will_not_resume_are_counted() {
    let fake = game();
    let frozen = Freeze::new(&fake, &[]).unwrap();
    fake.fault("resume", 0, Fault::Fail);
    assert_eq!(frozen.resume(), Err(ResumeError { failed: 1 }));
    assert_eq!(fake.suspend_counts(), [0, 1]);

    let fake = game();
    fake.fault("resume", 0, Fault::Fail);
    fake.fault("context", 0, Fault::Fail);
    let error = Freeze::new(&fake, &[]).unwrap_err();
    assert_eq!(error.unresumed, 1);
    assert!(!error.transient());
    assert_eq!(error.recovery(), RecoveryAction::RestartGame);
}
