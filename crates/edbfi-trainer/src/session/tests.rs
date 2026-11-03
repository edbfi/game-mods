// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]

use std::panic::{AssertUnwindSafe, catch_unwind};

use super::fake::{BASE, Block, FakeGame};
use super::*;
use crate::control::{ControlBit, ControlSet};
use crate::patch::fake::Fault;
use crate::patch::freeze::ResumeError;
use crate::patch::{PatchError, SiteFault};

fn candidate(game: &FakeGame) -> Candidate<Write> {
    Candidate::new(Box::new(game.clone()))
}

fn identified(game: &FakeGame, profile: &ValidProfile<'static>) -> Session<Identified> {
    identify(candidate(game), profile).unwrap()
}

fn prepared(game: &FakeGame, profile: &ValidProfile<'static>) -> Session<Prepared> {
    identified(game, profile)
        .claim()
        .unwrap()
        .prepare()
        .unwrap()
}

fn installed(game: &FakeGame, profile: &ValidProfile<'static>) -> Session<Installed> {
    prepared(game, profile).install().unwrap()
}

fn mask(bits: &[u8]) -> DesiredMask {
    DesiredMask::new(bits.iter().fold(ControlSet::EMPTY, |set, &bit| {
        set.with(ControlBit::new(bit))
    }))
}

/// The sites' jumps, read from the game.
fn jumps(game: &FakeGame, profile: &ValidProfile<'static>) -> Vec<Vec<u8>> {
    profile
        .profile()
        .sites
        .iter()
        .map(|site| {
            game.fake
                .peek(BASE + u64::from(site.rva.get()), site.original.len())
        })
        .collect()
}

#[test]
fn a_session_installs_publishes_and_restores_the_game_exactly() {
    let (game, profile) = FakeGame::new();
    let code = game.code();
    let mut session = installed(&game, &profile);
    assert_eq!(session.image(), Va::new(BASE));
    assert_eq!(game.blocks(), [Block::Retained]);
    for (jump, site) in jumps(&game, &profile).iter().zip(profile.profile().sites) {
        assert_eq!(jump[0], 0xE9, "{}", site.id);
        assert_eq!(jump.len(), site.original.len());
    }
    assert_eq!(game.info.word.get(), 0);
    // The caves are execute-read, and each jump lands on its own page of the block.
    let block = session.state.hooks.plan.allocation().get();
    assert_eq!(
        game.fake.protection(block),
        crate::patch::Protection::EXECUTE_READ
    );

    let first = session.publish(mask(&[0, 31]));
    assert_eq!(game.info.word.get(), 0x8000_0001);
    assert_eq!(first.revision(), 1);
    let stores = game.info.stores.get();
    let again = session.publish(mask(&[0, 31]));
    assert_eq!(
        game.info.stores.get(),
        stores,
        "an unchanged mask is not stored again"
    );
    assert_eq!(again.revision(), 2);
    assert_eq!(again.mask().bits(), mask(&[0, 31]).bits());
    assert_eq!(session.published(), mask(&[0, 31]));
    // A shutdown callback zeroed the word behind the session's back: the same mask is stored again.
    game.info.word.set(0);
    session.publish(mask(&[0, 31]));
    assert_eq!(game.info.word.get(), 0x8000_0001);

    let detached = session.restore(VerifyRestore::Text).unwrap();
    assert_eq!(game.code(), code);
    assert_eq!(game.info.word.get(), 0);
    assert_eq!(game.info.finished.get(), 1);
    // The caves were published, so the game keeps its view of the control word.
    assert!(!game.info.withdrawn.get());
    assert_eq!(detached.report().text, TextCheck::Identical);
    assert_eq!(game.fake.suspend_counts(), [0, 0]);
    // The caves outlive the hooks; the trainer's own view of the controls does not.
    assert_eq!(game.blocks(), [Block::Retained]);
    assert!(!game.info.controls_live.get());
    // Until the session ends no other trainer may claim the game.
    assert_eq!(
        identified(&game.second(), &profile)
            .claim()
            .unwrap_err()
            .error
            .recovery(),
        RecoveryAction::CloseOtherTrainer
    );
    drop(detached);
    identified(&game.second(), &profile).claim().unwrap();
}

#[test]
fn a_dropped_prepared_session_frees_its_caves() {
    let (game, profile) = FakeGame::new();
    let session = prepared(&game, &profile);
    assert_eq!(game.blocks(), [Block::Live]);
    drop(session);
    assert_eq!(game.blocks(), [Block::Freed]);
    // Nothing could read the control word, so its view left the game too.
    assert!(game.info.withdrawn.get());
}

#[test]
fn identification_names_each_mismatch() {
    let (game, profile) = FakeGame::new();
    let rejected = |game: &FakeGame| identify(candidate(game), &profile).unwrap_err().error;

    game.info.file.borrow_mut()[0x500] ^= 1;
    assert!(matches!(rejected(&game), IdentifyError::Unsupported { .. }));
    assert_eq!(rejected(&game).recovery(), RecoveryAction::UnsupportedBuild);

    let (game, _) = FakeGame::new();
    game.info.module.borrow_mut().path = PathBuf::from(r"C:\elsewhere\synthetic.exe");
    assert_eq!(rejected(&game), IdentifyError::ModulePath);

    let (game, _) = FakeGame::new();
    game.info.module.borrow_mut().size = 0x4000;
    assert!(matches!(
        rejected(&game),
        IdentifyError::ModuleSize { found: 0x4000, .. }
    ));

    // A loaded header that differs from the file's.
    let (game, _) = FakeGame::new();
    game.fake.poke(BASE + 0x80 + 4 + 20 + 56, &[0x01]);
    assert!(matches!(rejected(&game), IdentifyError::Identity(_)));
    assert_eq!(rejected(&game).recovery(), RecoveryAction::ModifiedGameCode);

    // Code still packed: every site is reported, and a later try may succeed.
    let (game, _) = FakeGame::new();
    let s3 = BASE + 0x1400;
    game.fake.poke(s3, &[0x90]);
    let error = rejected(&game);
    assert!(error.transient());
    let sites = error.sites().unwrap();
    assert_eq!(sites.len(), 4);
    assert_eq!(sites[2].status, SiteStatus::ContextMissing);
    assert!(
        sites
            .iter()
            .enumerate()
            .all(|(i, check)| i == 2 || check.status == SiteStatus::Verified)
    );
    // An unreadable process is retryable, not a verdict.
    let (game, _) = FakeGame::new();
    game.fake.fault("main_module", 0, Fault::Fail);
    assert_eq!(rejected(&game).recovery(), RecoveryAction::Retry);
}

#[test]
fn site_checks_tell_moved_and_ambiguous_signatures_apart() {
    let site = &crate::profile::synthetic::SYNTHETIC.sites[0];
    let rva = crate::addr::Rva::new(0x1000);
    let mut text = vec![0xCC; 0x1000];
    let context: Vec<u8> = site
        .context
        .aob
        .tokens()
        .iter()
        .map(|token| match token {
            crate::aob::Token::Byte(byte) => *byte,
            crate::aob::Token::Any => 0,
        })
        .collect();
    text[0x100..0x110].copy_from_slice(&context);
    assert_eq!(identify::check_site(site, &text, rva), SiteStatus::Verified);
    text[0x100] = 0xCC;
    text[0x300..0x310].copy_from_slice(&context);
    assert_eq!(
        identify::check_site(site, &text, rva),
        SiteStatus::ContextMoved {
            found: crate::addr::Rva::new(0x1300)
        }
    );
    text[0x100..0x110].copy_from_slice(&context);
    assert_eq!(
        identify::check_site(site, &text, rva),
        SiteStatus::ContextAmbiguous
    );
}

#[test]
fn placement_moves_past_taken_bases_and_gives_up_cleanly() {
    let (game, profile) = FakeGame::new();
    let claimed = identified(&game, &profile).claim().unwrap();
    // Every candidate of the first walk is taken by someone else.
    let reach = crate::cave::place::Reach::new(profile.profile(), Va::new(BASE)).unwrap();
    let first = crate::cave::place::candidates(
        reach,
        0x1_0000,
        prepare::free_regions(&game, reach).unwrap(),
    );
    assert!(first.len() > 1);
    game.info.taken.borrow_mut().insert(first[0].get());
    let prepared = claimed.prepare().unwrap();
    assert_eq!(prepared.state.hooks.plan.allocation(), first[1]);
    drop(prepared);

    let (game, profile) = FakeGame::new();
    for nth in 0..10_000 {
        game.fake.fault("allocate", nth, Fault::Fail);
    }
    let failure = identified(&game, &profile)
        .claim()
        .unwrap()
        .prepare()
        .unwrap_err();
    assert_eq!(failure.error, TrainerError::Prepare(PrepareError::NoSpace));
    assert_eq!(failure.error.recovery(), RecoveryAction::Retry);
    assert!(game.blocks().is_empty());

    // Any other allocation failure is not a race, so it is reported rather than retried elsewhere.
    let (game, profile) = FakeGame::new();
    game.info.deny.set(true);
    let failure = identified(&game, &profile)
        .claim()
        .unwrap()
        .prepare()
        .unwrap_err();
    assert!(matches!(
        failure.error,
        TrainerError::Prepare(PrepareError::Allocate(_))
    ));
    assert_eq!(game.fake.calls("allocate"), 1);
}

#[test]
fn a_failed_preparation_frees_the_caves_and_keeps_the_claim() {
    for (call, nth) in [
        ("write", 0),
        ("write", 3),
        ("read", 0),
        ("protect", 0),
        ("flush", 0),
    ] {
        let (game, profile) = FakeGame::new();
        let claimed = identified(&game, &profile).claim().unwrap();
        game.fake.fault_next(call, nth, Fault::Fail);
        let failure = claimed.prepare().unwrap_err();
        assert!(matches!(failure.error, TrainerError::Prepare(_)), "{call}");
        assert_eq!(game.blocks(), [Block::Freed], "{call}");
        assert!(game.info.withdrawn.get(), "{call}");
        game.fake.clear_faults();
        failure.session.prepare().unwrap();
    }
    let (game, profile) = FakeGame::new();
    game.fake.fault("controls", 0, Fault::Fail);
    let failure = identified(&game, &profile)
        .claim()
        .unwrap()
        .prepare()
        .unwrap_err();
    assert!(matches!(
        failure.error,
        TrainerError::Prepare(PrepareError::Controls(_))
    ));
}

#[test]
fn a_publication_after_a_shutdown_or_stop_request_publishes_zero() {
    for request in [
        |game: &FakeGame| game.info.interrupted.set(true),
        |game: &FakeGame| game.info.stop.set(true),
    ] {
        let (game, profile) = FakeGame::new();
        let mut session = installed(&game, &profile);
        session.publish(mask(&[0, 3]));
        assert_eq!(game.info.word.get(), 0b1001);
        // A shutdown callback zeroes the word, then the owner publishes a change it was already making.
        request(&game);
        game.info.word.set(0);
        let ack = session.publish(mask(&[0, 3, 5]));
        assert_eq!(game.info.word.get(), 0);
        assert_eq!(ack.mask().bits(), DesiredMask::ZERO.bits());
        assert_eq!(session.published(), DesiredMask::ZERO);
        session.restore(VerifyRestore::Sites).unwrap();
    }
}

#[test]
fn a_busy_thread_is_waited_for_and_a_stop_ends_the_wait() {
    let (game, profile) = FakeGame::new();
    let session = prepared(&game, &profile);
    let site = BASE + 0x1100;
    game.fake.0.borrow_mut().threads.get_mut(&2).unwrap().rip = Va::new(site + 2);
    game.info.exit_after_waits.set(None);
    // The thread stays inside a site: every attempt fails, and nothing is written.
    let failure = session.install().unwrap_err();
    assert!(matches!(failure.error, TrainerError::Freeze(_)));
    assert_eq!(game.info.waits.get(), FREEZE_ATTEMPTS - 1);
    assert!(failure.session.hooked().is_empty());
    assert_eq!(
        game.fake.calls("write"),
        4,
        "only the four caves were written"
    );
    let detached = failure.session.restore(VerifyRestore::Sites).unwrap();
    assert_eq!(game.blocks(), [Block::Freed], "unreached caves are freed");
    drop(detached);

    let (game, profile) = FakeGame::new();
    let session = prepared(&game, &profile);
    game.fake.0.borrow_mut().threads.get_mut(&2).unwrap().rip = Va::new(site);
    game.info.stop.set(true);
    let failure = session.install().unwrap_err();
    assert_eq!(failure.error, TrainerError::Stopped);
    assert_eq!(game.info.waits.get(), 1);

    let (game, profile) = FakeGame::new();
    let session = prepared(&game, &profile);
    game.fake.0.borrow_mut().threads.get_mut(&2).unwrap().rip = Va::new(site);
    game.info.exit_after_waits.set(Some(3));
    let failure = session.install().unwrap_err();
    assert_eq!(failure.error, TrainerError::Exited);
    let proof = failure.session.exit_proof().unwrap();
    let gone = failure.session.exited(proof).unwrap();
    drop(gone);
    assert!(!game.info.controls_live.get());
}

#[test]
fn a_thread_leaving_the_site_lets_installation_proceed() {
    let (game, profile) = FakeGame::new();
    let session = prepared(&game, &profile);
    // Inside S4 for two freezes, then elsewhere.
    let inside = Va::new(BASE + 0x1502);
    game.fake
        .0
        .borrow_mut()
        .moves
        .insert(2, [inside, inside, Va::new(BASE + 0x1900)].into());
    let installed = session.install().unwrap();
    assert_eq!(game.info.waits.get(), 2);
    drop(installed);
}

/// Installs and restores with `fault` on the `nth` call to `call`, then restores again without faults. The
/// game always ends exactly as it began, with its threads running.
fn survives(call: &'static str, nth: usize, fault: Fault) {
    let (game, profile) = FakeGame::new();
    let code = game.code();
    let session = prepared(&game, &profile);
    game.fake.fault_next(call, nth, fault);
    let context = format!("{fault:?} on {call} #{nth}");
    let recovery = match catch_unwind(AssertUnwindSafe(|| session.install())) {
        Ok(Ok(installed)) => {
            let Installed { owner, hooks, .. } = installed.state;
            at(installed.core, Recovery { owner, hooks })
        }
        Ok(Err(failure)) => failure.session,
        Err(_) => panic!("{context}: a panic escaped the install"),
    };
    assert_eq!(game.fake.suspend_counts(), [0, 0], "{context}");
    let recovery = match recovery.restore(VerifyRestore::Sites) {
        Ok(detached) => {
            assert_eq!(game.code(), code, "{context}");
            return drop(detached);
        }
        Err(failure) => failure.session,
    };
    game.fake.clear_faults();
    recovery
        .restore(VerifyRestore::Sites)
        .unwrap_or_else(|failure| panic!("{context}: {}", failure.error));
    assert_eq!(game.code(), code, "{context}");
    assert_eq!(game.fake.suspend_counts(), [0, 0], "{context}");
    assert_eq!(game.info.word.get(), 0, "{context}");
}

#[test]
fn installation_survives_a_failure_or_panic_at_every_call() {
    for call in [
        "read",
        "query",
        "protect",
        "write",
        "flush",
        "threads",
        "open_thread",
        "suspend",
        "context",
    ] {
        for nth in 0..40 {
            survives(call, nth, Fault::Fail);
            survives(call, nth, Fault::Panic);
        }
    }
    for nth in 0..8 {
        for written in 0..6 {
            survives("write", nth, Fault::Partial(written));
        }
    }
}

#[test]
fn foreign_bytes_keep_a_site_hooked_until_they_go() {
    let (game, profile) = FakeGame::new();
    let code = game.code();
    let session = installed(&game, &profile);
    let s2 = BASE + 0x1200;
    let jump = game.fake.peek(s2, 5);
    game.fake.poke(s2 + 1, &[0x00]);
    let failure = session.restore(VerifyRestore::Text).unwrap_err();
    let TrainerError::Restore(left) = &failure.error else {
        panic!("{}", failure.error);
    };
    assert_eq!(left.sites.len(), 1);
    assert_eq!(left.sites[0].site, SiteId(2));
    assert_eq!(left.sites[0].fault, SiteFault::Foreign);
    assert_eq!(failure.error.recovery(), RecoveryAction::RestartGame);
    assert_eq!(failure.session.hooked(), [SiteId(2)]);
    assert_eq!(game.info.word.get(), 0);
    game.fake.poke(s2, &jump);
    let detached = failure.session.restore(VerifyRestore::Sites).unwrap();
    assert_eq!(game.code(), code);
    assert_eq!(
        detached.report().text,
        TextCheck::Skipped(SkipReason::NotRequested)
    );
}

#[test]
fn a_whole_code_check_reports_changes_outside_the_sites() {
    let (game, profile) = FakeGame::new();
    let session = installed(&game, &profile);
    game.fake.poke(BASE + 0x1A00, &[0x90]);
    let detached = session.restore(VerifyRestore::Text).unwrap();
    assert_eq!(detached.report().text, TextCheck::Differs);
}

#[test]
fn a_game_that_exited_is_not_restored() {
    let (game, profile) = FakeGame::new();
    let mut session = installed(&game, &profile);
    session.publish(mask(&[1]));
    game.info.exited.set(true);
    let writes = game.fake.calls("write");
    let failure = session.restore(VerifyRestore::Text).unwrap_err();
    assert_eq!(failure.error, TrainerError::Exited);
    assert_eq!(game.fake.calls("write"), writes);
    assert_eq!(game.info.word.get(), 0);
    let (other, _) = FakeGame::new();
    other.info.exited.set(true);
    let foreign = identified(&other, &profile).exit_proof().unwrap();
    let session = failure.session.exited(foreign).unwrap_err();
    let proof = session.exit_proof().unwrap();
    session.exited(proof).unwrap();
}

#[test]
fn a_changed_original_refuses_installation_without_writing() {
    let (game, profile) = FakeGame::new();
    let session = prepared(&game, &profile);
    game.fake.poke(BASE + 0x1500, &[0x90]);
    let writes = game.fake.calls("write");
    let failure = session.install().unwrap_err();
    let TrainerError::Apply(error) = &failure.error else {
        panic!("{}", failure.error);
    };
    assert_eq!(error.cause, PatchError::Conflict { site: SiteId(4) });
    assert_eq!(failure.error.recovery(), RecoveryAction::ModifiedGameCode);
    assert_eq!(game.fake.calls("write"), writes);
    assert!(failure.session.hooked().is_empty());
    // Nothing was written, so nothing could reach the caves: they are not retained.
    assert_eq!(game.blocks(), [Block::Live]);
    drop(failure);
    assert_eq!(game.blocks(), [Block::Freed]);
}

#[test]
fn a_thread_that_will_not_resume_blocks_detach_until_it_does() {
    let (game, profile) = FakeGame::new();
    let session = installed(&game, &profile);
    // Restoration works, but one thread then refuses to resume, twice.
    game.fake.fault_next("resume", 0, Fault::Fail);
    let failure = session.restore(VerifyRestore::Sites).unwrap_err();
    assert_eq!(
        failure.error,
        TrainerError::Resume(ResumeError { failed: 1 })
    );
    assert_eq!(failure.error.recovery(), RecoveryAction::RestartGame);
    assert!(failure.session.hooked().is_empty());
    assert_eq!(failure.session.suspended_threads(), 1);
    assert_eq!(game.fake.suspend_counts(), [0, 1]);
    game.fake.fault_next("resume", 0, Fault::Fail);
    let failure = failure.session.restore(VerifyRestore::Sites).unwrap_err();
    assert_eq!(failure.session.suspended_threads(), 1);
    // Once it resumes, the session detaches with the game running.
    failure.session.restore(VerifyRestore::Sites).unwrap();
    assert_eq!(game.fake.suspend_counts(), [0, 0]);
}

#[test]
fn a_restore_that_succeeds_on_a_retry_is_a_success() {
    let (game, profile) = FakeGame::new();
    let code = game.code();
    let session = installed(&game, &profile);
    // The first restoring write fails; the retry inside the same freeze succeeds.
    game.fake.fault_next("write", 0, Fault::Fail);
    let detached = session.restore(VerifyRestore::Text).unwrap();
    assert_eq!(game.code(), code);
    assert_eq!(detached.report().text, TextCheck::Identical);
}

#[test]
fn an_install_rolled_back_on_a_retry_reports_a_clean_rollback() {
    let (game, profile) = FakeGame::new();
    let session = prepared(&game, &profile);
    // The second jump's write fails; the rollback's first write fails too, and its retry succeeds.
    game.fake.fault_next("write", 1, Fault::Fail);
    game.fake.fault_next("write", 2, Fault::Fail);
    let failure = session.install().unwrap_err();
    let TrainerError::Apply(error) = &failure.error else {
        panic!("{}", failure.error);
    };
    assert_eq!(error.rollback, Ok(()));
    assert_eq!(failure.error.recovery(), RecoveryAction::Retry);
    assert!(failure.session.hooked().is_empty());
}

#[test]
fn a_panic_whose_rollback_fails_reports_the_hooks_left() {
    let (game, profile) = FakeGame::new();
    let session = prepared(&game, &profile);
    // The second jump's write panics after the first jump went in; every rollback write then fails.
    game.fake.fault_next("write", 1, Fault::Panic);
    for nth in 2..64 {
        game.fake.fault_next("write", nth, Fault::Fail);
    }
    let failure = session.install().unwrap_err();
    assert!(
        matches!(failure.error, TrainerError::Restore(_)),
        "{}",
        failure.error
    );
    assert_eq!(failure.error.recovery(), RecoveryAction::RestartGame);
    assert_eq!(failure.session.hooked(), [SiteId(1)]);
    game.fake.clear_faults();
    failure.session.restore(VerifyRestore::Sites).unwrap();
}

#[test]
fn an_owed_resume_survives_a_restore_whose_own_freeze_resumes() {
    let (game, profile) = FakeGame::new();
    let session = prepared(&game, &profile);
    // Install leaves one thread owing a resume.
    game.fake.fault_next("resume", 0, Fault::Fail);
    let failure = session.install().unwrap_err();
    assert_eq!(failure.session.suspended_threads(), 1);
    // The restore's retry of that resume fails again, while its own freeze resumes cleanly.
    game.fake.fault_next("resume", 0, Fault::Fail);
    let failure = failure.session.restore(VerifyRestore::Sites).unwrap_err();
    assert_eq!(
        failure.error,
        TrainerError::Resume(ResumeError { failed: 1 })
    );
    assert!(failure.session.hooked().is_empty());
    assert_eq!(game.fake.suspend_counts(), [0, 1]);
    failure.session.restore(VerifyRestore::Sites).unwrap();
    assert_eq!(game.fake.suspend_counts(), [0, 0]);
}

#[test]
fn the_control_word_goes_above_8_gib_and_away_from_the_image() {
    let (game, profile) = FakeGame::new();
    // Someone already holds the first boundary above 8 GiB.
    game.fake
        .map(1 << 33, 0x1000, crate::patch::Protection::READ_WRITE, 0);
    let session = prepared(&game, &profile);
    let at = game.info.controls_at.get();
    assert_eq!(at, (1 << 33) + 0x1_0000);
    assert!(at.abs_diff(BASE) >= 1 << 32);
    drop(session);
}

#[test]
fn a_rolled_back_install_still_gets_its_whole_code_check() {
    let (game, profile) = FakeGame::new();
    let session = prepared(&game, &profile);
    game.fake.fault_next("write", 1, Fault::Fail);
    let failure = session.install().unwrap_err();
    assert!(failure.session.hooked().is_empty());
    let detached = failure.session.restore(VerifyRestore::Text).unwrap();
    assert_eq!(detached.report().text, TextCheck::Identical);
    // An install that never wrote has nothing to check.
    let (game, profile) = FakeGame::new();
    let session = prepared(&game, &profile);
    game.info.stop.set(true);
    game.fake.0.borrow_mut().threads.get_mut(&2).unwrap().rip = Va::new(BASE + 0x1100);
    let failure = session.install().unwrap_err();
    let detached = failure.session.restore(VerifyRestore::Text).unwrap();
    assert_eq!(
        detached.report().text,
        TextCheck::Skipped(SkipReason::Untouched)
    );
}

#[test]
fn a_lost_race_for_the_control_word_tries_the_next_boundary() {
    let (game, profile) = FakeGame::new();
    // The first boundary looks free to the walk but is taken when mapped; the next one in the same region
    // is used.
    game.info.taken.borrow_mut().insert(1 << 33);
    drop(prepared(&game, &profile));
    assert_eq!(game.info.controls_at.get(), (1 << 33) + 0x1_0000);
    // Any other failure is reported.
    let (game, profile) = FakeGame::new();
    game.fake.fault("controls", 0, Fault::Fail);
    let failure = identified(&game, &profile)
        .claim()
        .unwrap()
        .prepare()
        .unwrap_err();
    assert!(matches!(
        failure.error,
        TrainerError::Prepare(PrepareError::Controls(_))
    ));
}
