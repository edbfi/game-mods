// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::control::{ControlBit, ControlSet, DesiredMask};
use crate::patch::Process as _;
use crate::session::fake::{BASE, FakeGame};
use crate::testdir::TestDir;

/// Pretend discovery: the games listed, opened as the pretend game.
#[derive(Clone, Default)]
struct Games {
    pids: Rc<RefCell<Vec<u32>>>,
    game: Rc<RefCell<Option<FakeGame>>>,
    deny: Rc<RefCell<bool>>,
}

impl Games {
    fn running(game: &FakeGame) -> Self {
        let games = Self::default();
        games.set(&[game.fake.pid()]);
        *games.game.borrow_mut() = Some(game.clone());
        games
    }

    fn set(&self, pids: &[u32]) {
        *self.pids.borrow_mut() = pids.to_vec();
    }
}

impl Discover for Games {
    fn find(&self) -> Result<Vec<u32>, OsError> {
        Ok(self.pids.borrow().clone())
    }

    fn open(&self, _pid: u32) -> Result<Candidate<Write>, OsError> {
        if *self.deny.borrow() {
            return Err(OsError::new("OpenProcess", 5));
        }
        let game = self.game.borrow().clone().expect("a game to open");
        Ok(Candidate::new(Box::new(game)))
    }
}

struct Harness {
    runtime: Runtime,
    started: Started,
    _dir: TestDir,
}

fn start(games: &Games, profile: &ValidProfile<'static>, options: Options) -> Harness {
    let dir = TestDir::new("runtime");
    let locations = Locations {
        settings: dir.path().join("config"),
        logs: dir.path().join("logs"),
    };
    let journal = Journal::open(&locations.logs, "runtime test");
    let (runtime, started) = Runtime::start(
        profile.clone(),
        Box::new(games.clone()),
        Ok(locations),
        journal,
        options,
    );
    Harness {
        runtime,
        started,
        _dir: dir,
    }
}

/// Runs every step that is due now, returning the phases it reported.
fn settle(runtime: &mut Runtime) -> Vec<Phase> {
    let mut phases = Vec::new();
    for _ in 0..32 {
        let due = runtime.due();
        if due.is_none_or(|at| at > Instant::now()) {
            break;
        }
        runtime.step();
        phases.extend(phases_of(runtime.events()));
    }
    phases
}

fn phases_of(events: Vec<RuntimeEvent>) -> Vec<Phase> {
    events
        .into_iter()
        .filter_map(|event| match event {
            RuntimeEvent::Phase(phase) => Some(phase),
            _ => None,
        })
        .collect()
}

fn mask(bits: &[u8]) -> DesiredMask {
    DesiredMask::new(bits.iter().fold(ControlSet::EMPTY, |set, &bit| {
        set.with(ControlBit::new(bit))
    }))
}

/// How many writes preparing the pretend game takes, counted on a twin.
fn preparing() -> usize {
    let (twin, profile) = FakeGame::new();
    let _prepared = crate::session::identify(Candidate::new(Box::new(twin.clone())), &profile)
        .unwrap()
        .claim()
        .unwrap()
        .prepare()
        .unwrap();
    twin.fake.calls("write")
}

fn attached(phase: &Phase) -> bool {
    phase.is_attached()
}

#[test]
fn a_first_run_writes_defaults_and_waits_for_the_game() {
    let (_, profile) = FakeGame::new();
    let games = Games::default();
    let mut h = start(&games, &profile, Options::default());
    assert_eq!(h.started.phase, Phase::Waiting);
    assert_eq!(h.started.settings, Settings::defaults(&profile));
    let path = h.started.settings_path.clone().unwrap();
    assert!(path.exists());
    assert!(h.started.log_path.as_ref().unwrap().exists());
    // The start is not reported again; only what the journal recorded meanwhile is.
    let events = h.runtime.events();
    assert!(!events.is_empty());
    assert!(
        events
            .iter()
            .all(|event| matches!(event, RuntimeEvent::Journal(_)))
    );
    // Nothing runs: one look, then the next in a second.
    assert!(settle(&mut h.runtime).is_empty());
    let next = h.runtime.due().unwrap();
    assert!(next > Instant::now() + DISCOVERY - Duration::from_millis(200));
    assert!(next <= Instant::now() + DISCOVERY);
}

#[test]
fn a_game_is_verified_hooked_published_and_restored_on_quit() {
    let (game, profile) = FakeGame::new();
    let code = game.code();
    let games = Games::running(&game);
    let mut h = start(&games, &profile, Options::default());
    let phases = settle(&mut h.runtime);
    assert_eq!(
        phases[..2],
        [
            Phase::Verifying {
                stage: VerifyStage::Executable
            },
            Phase::Installing
        ]
    );
    assert!(attached(&phases[2]), "{phases:?}");
    assert_eq!(h.runtime.game(), Some(game.fake.pid()));
    assert_eq!(h.runtime.due(), None, "attached, nothing is timed");
    assert_ne!(game.code(), code);

    h.runtime.command(RuntimeCommand::Publish(mask(&[0, 3])));
    let events = h.runtime.events();
    let [RuntimeEvent::Ack(ack)] = events.as_slice() else {
        panic!("{events:?}");
    };
    assert_eq!(ack.mask().bits(), mask(&[0, 3]).bits());
    assert_eq!(game.info.word.get(), 0b1001);

    h.runtime.command(RuntimeCommand::Quit);
    assert_eq!(phases_of(h.runtime.events()), [Phase::Restoring]);
    assert_eq!(settle(&mut h.runtime), [Phase::Finished(Finish::Restored)]);
    assert_eq!(h.runtime.done(), Some(&Done::Finished(Finish::Restored)));
    assert_eq!(game.code(), code);
    assert_eq!(game.info.word.get(), 0);
    assert!(!game.info.controls_live.get(), "the session was released");
}

#[test]
fn a_publication_that_cannot_reach_the_game_is_journaled_and_reported_failed() {
    let (game, profile) = FakeGame::new();
    let games = Games::running(&game);
    let mut h = start(&games, &profile, Options::default());
    settle(&mut h.runtime);
    game.info.store_fails.set(true);
    h.runtime.command(RuntimeCommand::Publish(mask(&[0])));
    let events = h.runtime.events();
    let [RuntimeEvent::Journal(entry), RuntimeEvent::PublishFailed] = events.as_slice() else {
        panic!("{events:?}");
    };
    assert_eq!(entry.event.severity, crate::journal::Severity::Error);
    assert_eq!(entry.event.kind, EntryKind::Hooks);
    // The session stays attached: the next publication that lands is acknowledged.
    game.info.store_fails.set(false);
    h.runtime.command(RuntimeCommand::Publish(mask(&[3])));
    let events = h.runtime.events();
    let [RuntimeEvent::Ack(ack)] = events.as_slice() else {
        panic!("{events:?}");
    };
    assert_eq!(ack.mask().bits(), mask(&[3]).bits());
    assert_eq!(game.info.word.get(), 0b1000);
    // A game that exited ends the session through its exit notice, so its failed publication is not news.
    game.info.store_fails.set(true);
    game.info.exited.set(true);
    h.runtime.command(RuntimeCommand::Publish(mask(&[0])));
    assert_eq!(h.runtime.events(), [RuntimeEvent::PublishFailed]);
}

#[test]
fn publications_are_ignored_until_the_game_is_hooked() {
    let (game, profile) = FakeGame::new();
    let games = Games::default();
    let mut h = start(&games, &profile, Options::default());
    h.runtime.events();
    h.runtime.command(RuntimeCommand::Publish(mask(&[0])));
    assert!(h.runtime.events().is_empty());
    assert_eq!(game.info.word.get(), 0xFFFF_FFFF);
}

#[test]
fn a_code_mismatch_is_retried_while_the_game_unpacks() {
    let (game, profile) = FakeGame::new();
    let code = game.code();
    let site = BASE + 0x1200;
    game.fake.poke(site, &[0x00]);
    let games = Games::running(&game);
    let mut h = start(&games, &profile, Options::default());
    let phases = settle(&mut h.runtime);
    assert_eq!(
        phases.last(),
        Some(&Phase::Verifying {
            stage: VerifyStage::Unpacking
        })
    );
    let due = h.runtime.due().unwrap();
    assert!(due > Instant::now() + UNPACK_RETRY / 2);
    // Still packed: tried again, same phase, nothing reported.
    h.runtime.step();
    assert!(phases_of(h.runtime.events()).is_empty());
    game.fake.poke(site, &code[0x200..0x201]);
    h.runtime.step();
    let phases = phases_of(h.runtime.events());
    assert_eq!(phases, [Phase::Installing]);
    assert!(attached(&settle(&mut h.runtime)[0]));
}

#[test]
fn a_game_that_exits_returns_to_waiting_and_is_found_again() {
    let (game, profile) = FakeGame::new();
    let games = Games::running(&game);
    let mut h = start(&games, &profile, Options::default());
    settle(&mut h.runtime);
    h.runtime.command(RuntimeCommand::Publish(mask(&[1])));
    h.runtime.events();
    // A spurious wake changes nothing.
    h.runtime.exited();
    assert!(h.runtime.events().is_empty());
    game.info.exited.set(true);
    let writes = game.fake.calls("write");
    h.runtime.exited();
    assert_eq!(phases_of(h.runtime.events()), [Phase::Waiting]);
    assert_eq!(
        game.fake.calls("write"),
        writes,
        "a dead game is not written"
    );
    assert_eq!(h.runtime.game(), None);
    games.set(&[]);
    assert!(settle(&mut h.runtime).is_empty());
    h.runtime.command(RuntimeCommand::Quit);
    assert_eq!(h.runtime.done(), Some(&Done::Finished(Finish::Untouched)));
}

#[test]
fn several_games_refuse_until_one_is_left_or_one_is_chosen() {
    let (game, profile) = FakeGame::new();
    let games = Games::running(&game);
    let pid = game.fake.pid();
    games.set(&[pid, pid + 1]);
    let mut h = start(&games, &profile, Options::default());
    let phases = settle(&mut h.runtime);
    let [Phase::Failed(failure)] = phases.as_slice() else {
        panic!("{phases:?}");
    };
    assert_eq!(failure.recovery, RecoveryAction::CloseDuplicates);
    // Still several: looked at again a second later, without repeating the failure.
    assert!(h.runtime.due().is_some());
    h.runtime.step();
    assert!(phases_of(h.runtime.events()).is_empty());
    games.set(&[pid]);
    h.runtime.step();
    assert_eq!(
        phases_of(h.runtime.events()),
        [Phase::Verifying {
            stage: VerifyStage::Executable
        }]
    );

    let games = Games::running(&game);
    games.set(&[pid + 1, pid]);
    let (game, profile) = FakeGame::new();
    *games.game.borrow_mut() = Some(game);
    let mut h = start(
        &games,
        &profile,
        Options {
            pid: Some(pid),
            ..Options::default()
        },
    );
    assert!(attached(settle(&mut h.runtime).last().unwrap()));
}

#[test]
fn an_unopenable_game_fails_until_retried() {
    let (game, profile) = FakeGame::new();
    let games = Games::running(&game);
    *games.deny.borrow_mut() = true;
    let mut h = start(&games, &profile, Options::default());
    let phases = settle(&mut h.runtime);
    let [Phase::Failed(failure)] = phases.as_slice() else {
        panic!("{phases:?}");
    };
    assert!(failure.can_retry() && failure.message.contains("OpenProcess"));
    assert_eq!(h.runtime.due(), None, "no automatic retry");
    *games.deny.borrow_mut() = false;
    h.runtime.command(RuntimeCommand::Retry);
    assert_eq!(phases_of(h.runtime.events()), [Phase::Waiting]);
    assert!(attached(settle(&mut h.runtime).last().unwrap()));
    // Quitting from a failure card reports that failure.
    let (game, profile) = FakeGame::new();
    let games = Games::running(&game);
    *games.deny.borrow_mut() = true;
    let mut h = start(&games, &profile, Options::default());
    settle(&mut h.runtime);
    h.runtime.command(RuntimeCommand::Quit);
    assert!(matches!(h.runtime.done(), Some(Done::Failed(_))));
}

#[test]
fn a_failed_restore_pins_the_game_until_a_second_quit_gives_up() {
    let (game, profile) = FakeGame::new();
    let games = Games::running(&game);
    let mut h = start(&games, &profile, Options::default());
    settle(&mut h.runtime);
    let s2 = BASE + 0x1200;
    game.fake.poke(s2 + 1, &[0x00]);
    h.runtime.command(RuntimeCommand::Quit);
    let phases = [phases_of(h.runtime.events()), settle(&mut h.runtime)].concat();
    let [Phase::Restoring, Phase::Failed(failure)] = phases.as_slice() else {
        panic!("{phases:?}");
    };
    assert!(failure.left_hooks() && !failure.can_retry());
    assert_eq!(h.runtime.done(), None, "the user reads the failure first");
    assert_eq!(h.runtime.game(), None, "a pinned game gets no hotkeys");
    assert!(game.info.controls_live.get(), "ownership is kept");
    // Retrying is refused while hooks remain, and a pinned game is never looked for again.
    h.runtime.command(RuntimeCommand::Retry);
    assert!(h.runtime.events().is_empty());
    assert!(settle(&mut h.runtime).is_empty());
    h.runtime.command(RuntimeCommand::Quit);
    settle(&mut h.runtime);
    let Some(Done::HooksLeft { sites, .. }) = h.runtime.done() else {
        panic!("{:?}", h.runtime.done());
    };
    assert_eq!(sites.len(), 1);
}

#[test]
fn a_pinned_game_that_exits_is_released() {
    let (game, profile) = FakeGame::new();
    let games = Games::running(&game);
    let mut h = start(&games, &profile, Options::default());
    settle(&mut h.runtime);
    game.fake.poke(BASE + 0x1201, &[0x00]);
    h.runtime.command(RuntimeCommand::Quit);
    settle(&mut h.runtime);
    h.runtime.events();
    game.info.exited.set(true);
    h.runtime.exited();
    assert_eq!(phases_of(h.runtime.events()), [Phase::Waiting]);
    assert!(!game.info.controls_live.get());
}

#[test]
fn a_stop_request_restores_without_asking_and_a_failure_exits_at_once() {
    let (game, profile) = FakeGame::new();
    let code = game.code();
    let games = Games::running(&game);
    let mut h = start(&games, &profile, Options::default());
    settle(&mut h.runtime);
    h.runtime.stop();
    settle(&mut h.runtime);
    assert_eq!(h.runtime.done(), Some(&Done::Finished(Finish::Restored)));
    assert_eq!(game.code(), code);

    let (game, profile) = FakeGame::new();
    let games = Games::running(&game);
    let mut h = start(&games, &profile, Options::default());
    settle(&mut h.runtime);
    game.fake.poke(BASE + 0x1201, &[0x00]);
    h.runtime.shutdown();
    settle(&mut h.runtime);
    assert!(matches!(h.runtime.done(), Some(Done::HooksLeft { .. })));
}

#[test]
fn a_stop_while_waiting_finishes_untouched() {
    let (_, profile) = FakeGame::new();
    let games = Games::default();
    let mut h = start(&games, &profile, Options::default());
    h.runtime.shutdown();
    assert_eq!(
        phases_of(h.runtime.events()),
        [Phase::Finished(Finish::Untouched)]
    );
    assert_eq!(h.runtime.done(), Some(&Done::Finished(Finish::Untouched)));
    // Further steps and commands do nothing.
    h.runtime.step();
    h.runtime.command(RuntimeCommand::Retry);
    assert!(h.runtime.events().is_empty());
}

#[test]
fn a_quit_while_the_game_is_verified_never_hooks_it() {
    let (game, profile) = FakeGame::new();
    let code = game.code();
    let games = Games::running(&game);
    let mut h = start(&games, &profile, Options::default());
    h.runtime.step();
    assert_eq!(
        phases_of(h.runtime.events()),
        [Phase::Verifying {
            stage: VerifyStage::Executable
        }]
    );
    h.runtime.shutdown();
    settle(&mut h.runtime);
    assert_eq!(h.runtime.done(), Some(&Done::Finished(Finish::Untouched)));
    assert_eq!(game.code(), code);
}

#[test]
fn a_failed_install_rolls_back_and_can_be_retried() {
    let (game, profile) = FakeGame::new();
    let code = game.code();
    let games = Games::running(&game);
    let mut h = start(&games, &profile, Options::default());
    // The install writes one site, then fails.
    game.fake
        .fault_next("write", preparing() + 1, crate::patch::fake::Fault::Fail);
    let phases = settle(&mut h.runtime);
    let Some(Phase::Failed(failure)) = phases.last() else {
        panic!("{phases:?}");
    };
    assert!(!failure.left_hooks() && failure.can_retry(), "{failure:?}");
    assert_eq!(game.code(), code);
    assert!(!game.info.controls_live.get(), "nothing is kept");
    h.runtime.command(RuntimeCommand::Retry);
    assert!(attached(settle(&mut h.runtime).last().unwrap()));
}

#[test]
fn invalid_settings_block_until_reset_and_unusable_folders_allow_session_only() {
    let (game, profile) = FakeGame::new();
    let games = Games::running(&game);
    let dir = TestDir::new("runtime-settings");
    let locations = Locations {
        settings: dir.path().join("config"),
        logs: dir.path().join("logs"),
    };
    std::fs::create_dir_all(&locations.settings).unwrap();
    let file = locations.settings.join("settings.toml");
    std::fs::write(&file, "schema = 1\n[trainer]\nstart_paused = maybe\n").unwrap();
    let (mut runtime, started) = Runtime::start(
        profile.clone(),
        Box::new(games.clone()),
        Ok(locations),
        Journal::memory(),
        Options::default(),
    );
    assert!(matches!(
        started.phase,
        Phase::Settings(SettingsProblem::Invalid { .. })
    ));
    assert_eq!(runtime.due(), None, "nothing connects meanwhile");
    // Saving defaults over the user's file is refused while the problem stands.
    assert_eq!(
        runtime.save(&Settings::defaults(&profile)).unwrap(),
        Saved::Unchanged
    );
    runtime.command(RuntimeCommand::Retry);
    assert!(matches!(
        phases_of(runtime.events()).as_slice(),
        [Phase::Settings(_)]
    ));
    // An existing file's choices never silently become unsaved defaults.
    runtime.command(RuntimeCommand::SessionOnly);
    assert!(runtime.events().is_empty());
    runtime.command(RuntimeCommand::ResetSettings);
    let events = runtime.events();
    assert!(events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Loaded { path: Some(path), .. } if *path == file
    )));
    assert!(events.contains(&RuntimeEvent::Phase(Phase::Waiting)));
    assert!(attached(settle(&mut runtime).last().unwrap()));

    let (mut runtime, started) = Runtime::start(
        profile.clone(),
        Box::new(Games::default()),
        Err(LocationError::Unset { var: "APPDATA" }),
        Journal::memory(),
        Options::default(),
    );
    let Phase::Settings(SettingsProblem::Store { session_only, .. }) = started.phase else {
        panic!("{:?}", started.phase);
    };
    assert!(session_only && started.settings_path.is_none());
    runtime.command(RuntimeCommand::SessionOnly);
    let events = runtime.events();
    assert!(events.contains(&RuntimeEvent::Loaded {
        settings: Settings::defaults(&profile),
        path: None
    }));
    assert_eq!(
        runtime.save(&Settings::defaults(&profile)).unwrap(),
        Saved::Unchanged
    );
    runtime.command(RuntimeCommand::Quit);
    assert_eq!(runtime.done(), Some(&Done::Finished(Finish::Untouched)));
}

#[test]
fn saves_reach_the_file_and_warnings_reach_the_dashboard() {
    let (_, profile) = FakeGame::new();
    let mut h = start(&Games::default(), &profile, Options::default());
    let path = h.started.settings_path.clone().unwrap();
    let mut settings = h.started.settings.clone();
    settings.sounds = !settings.sounds;
    assert!(matches!(
        h.runtime.save(&settings).unwrap(),
        Saved::Written { external: None }
    ));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        settings.to_toml(&profile)
    );
    std::fs::write(&path, "edited outside").unwrap();
    settings.sounds = !settings.sounds;
    assert!(matches!(
        h.runtime.save(&settings).unwrap(),
        Saved::Written {
            external: Some(External::Kept(_))
        }
    ));
    let events = h.runtime.events();
    assert!(events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Journal(entry) if entry.event.severity == crate::journal::Severity::Warning
    )));
}

#[test]
fn a_game_that_never_finishes_unpacking_fails_at_the_deadline() {
    let (game, profile) = FakeGame::new();
    game.fake.poke(BASE + 0x1200, &[0x00]);
    let games = Games::running(&game);
    let mut h = start(&games, &profile, Options::default());
    h.runtime.unpack_deadline = Duration::ZERO;
    let phases = settle(&mut h.runtime);
    let Some(Phase::Failed(failure)) = phases.last() else {
        panic!("{phases:?}");
    };
    assert!(
        failure.message.contains("did not finish starting"),
        "{failure:?}"
    );
    assert_eq!(h.runtime.due(), None, "no endless retry");
}

#[test]
fn a_whole_code_mismatch_after_restoring_is_a_failure_unless_shutdown_skips_the_check() {
    let verify = Options {
        verify: VerifyRestore::Text,
        ..Options::default()
    };
    let (game, profile) = FakeGame::new();
    let games = Games::running(&game);
    let mut h = start(&games, &profile, verify);
    settle(&mut h.runtime);
    game.fake.poke(BASE + 0x1A00, &[0x90]);
    h.runtime.command(RuntimeCommand::Quit);
    settle(&mut h.runtime);
    let Some(Done::Failed(failure)) = h.runtime.done() else {
        panic!("{:?}", h.runtime.done());
    };
    assert_eq!(failure.recovery, RecoveryAction::ModifiedGameCode);
    assert!(
        !game.info.controls_live.get(),
        "the restored session is released"
    );

    // Shutting down skips the optional check, so the same game finishes restored.
    let (game, profile) = FakeGame::new();
    let games = Games::running(&game);
    let mut h = start(&games, &profile, verify);
    settle(&mut h.runtime);
    game.fake.poke(BASE + 0x1A00, &[0x90]);
    h.runtime.shutdown();
    settle(&mut h.runtime);
    assert_eq!(h.runtime.done(), Some(&Done::Finished(Finish::Restored)));
}

#[test]
fn a_step_that_never_finished_reports_that_hooks_may_remain() {
    let (_, profile) = FakeGame::new();
    let mut h = start(&Games::default(), &profile, Options::default());
    // As a panic inside a restore leaves it: the session was taken out and dropped mid-change.
    h.runtime.phase = Phase::Restoring;
    h.runtime.shutdown();
    assert!(matches!(h.runtime.done(), Some(Done::HooksLeft { .. })));
}

#[test]
fn a_stop_or_shutdown_while_hooks_are_written_exits_untouched() {
    for request in [
        |game: &FakeGame| game.info.stop.set(true),
        |game: &FakeGame| game.info.interrupted.set(true),
    ] {
        let (game, profile) = FakeGame::new();
        let code = game.code();
        let games = Games::running(&game);
        let mut h = start(&games, &profile, Options::default());
        h.runtime.step();
        h.runtime.step();
        assert_eq!(
            phases_of(h.runtime.events()).last(),
            Some(&Phase::Installing)
        );
        // A thread inside a site keeps the freeze failing, and the request arrives meanwhile.
        game.fake.0.borrow_mut().threads.get_mut(&2).unwrap().rip =
            crate::addr::Va::new(BASE + 0x1102);
        request(&game);
        h.runtime.step();
        assert_eq!(h.runtime.done(), Some(&Done::Finished(Finish::Untouched)));
        assert_eq!(game.code(), code);
        assert!(!game.info.controls_live.get());
    }
}

#[test]
fn a_whole_code_mismatch_after_a_stopped_install_is_still_a_failure() {
    let (game, profile) = FakeGame::new();
    let games = Games::running(&game);
    let mut h = start(
        &games,
        &profile,
        Options {
            verify: VerifyRestore::Text,
            ..Options::default()
        },
    );
    h.runtime.step();
    h.runtime.step();
    // The install writes one site, then fails.
    game.fake
        .fault_next("write", preparing() + 1, crate::patch::fake::Fault::Fail);
    game.info.stop.set(true);
    game.fake.poke(BASE + 0x1A00, &[0x90]);
    h.runtime.step();
    let Some(Done::Failed(failure)) = h.runtime.done() else {
        panic!("{:?}", h.runtime.done());
    };
    assert_eq!(failure.recovery, RecoveryAction::ModifiedGameCode);
}

#[test]
fn a_shutdown_pending_when_an_install_fails_skips_the_whole_code_check() {
    let (game, profile) = FakeGame::new();
    let games = Games::running(&game);
    let mut h = start(
        &games,
        &profile,
        Options {
            verify: VerifyRestore::Text,
            ..Options::default()
        },
    );
    h.runtime.step();
    h.runtime.step();
    game.fake
        .fault_next("write", preparing() + 1, crate::patch::fake::Fault::Fail);
    // A close notice arrived during the install; the owner has not heard it yet.
    game.info.interrupted.set(true);
    game.fake.poke(BASE + 0x1A00, &[0x90]);
    h.runtime.step();
    assert_eq!(h.runtime.done(), Some(&Done::Finished(Finish::Untouched)));
}
