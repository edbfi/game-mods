// SPDX-License-Identifier: AGPL-3.0-only
//! A pretend game for session tests: the synthetic profile's code loaded at a relocated base, with a matching
//! executable file and a profile whose fingerprints are that file's.
#![forbid(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use crate::addr::{Rva, Va};
use crate::identity::Sha256Digest;
use crate::patch::fake::{Fake, PAGE};
use crate::patch::{OsError, Process, Protection, Region, Thread};
use crate::pe::testing::{block, image};
use crate::profile::synthetic::SYNTHETIC;
use crate::profile::{Identity, ModSlug, Profile, TextSection, ValidProfile};

use super::{Allocation, ClaimError, Controls, Game, Module, Owner};

/// Where the pretend game is loaded; its file prefers `0x1_4000_0000`.
pub(crate) const BASE: u64 = 0x7FF6_1230_0000;
/// A code offset with a 64-bit relocation, away from every site.
const RELOCATED: usize = 0x800;

/// What happened to an allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    Live,
    Freed,
    Retained,
}

#[derive(Debug)]
pub(crate) struct Info {
    pub(crate) path: PathBuf,
    pub(crate) file: RefCell<Vec<u8>>,
    pub(crate) module: RefCell<Module>,
    pub(crate) exited: Cell<bool>,
    /// The game exits after this many waits.
    pub(crate) exit_after_waits: Cell<Option<u32>>,
    pub(crate) waits: Cell<u32>,
    pub(crate) word: Cell<u32>,
    pub(crate) stores: Cell<u32>,
    pub(crate) finished: Cell<u32>,
    pub(crate) controls_live: Cell<bool>,
    pub(crate) stop: Cell<bool>,
    /// Whether this trainer was asked to shut down.
    pub(crate) interrupted: Cell<bool>,
    /// Whether the game's view of the control word was unmapped.
    pub(crate) withdrawn: Cell<bool>,
    /// Where the control word was mapped.
    pub(crate) controls_at: Cell<u64>,
    /// Allocation fails with access denied.
    pub(crate) deny: Cell<bool>,
    pub(crate) blocks: RefCell<Vec<(u64, Block)>>,
    /// Bases that look free to a walk but fail to allocate, as if another allocation took them.
    pub(crate) taken: RefCell<BTreeSet<u64>>,
}

/// The pretend game: its memory and threads, and everything else a session asks.
#[derive(Clone, Debug)]
pub(crate) struct FakeGame {
    pub(crate) fake: Fake,
    pub(crate) info: Rc<Info>,
    owners: Rc<RefCell<BTreeSet<u32>>>,
}

/// The synthetic code section: filler with each site's signature in place.
fn text() -> Vec<u8> {
    let mut text = vec![0xCC; SYNTHETIC.identity.text.size as usize];
    let start = SYNTHETIC.identity.text.rva.get() as usize;
    for site in SYNTHETIC.sites {
        let at = site.context.rva.get() as usize - start;
        for (i, token) in site.context.aob.tokens().iter().enumerate() {
            text[at + i] = match token {
                crate::aob::Token::Byte(byte) => *byte,
                crate::aob::Token::Any => 0x0C,
            };
        }
    }
    text[RELOCATED..RELOCATED + 8].copy_from_slice(&0x1_4000_2010u64.to_le_bytes());
    text
}

impl FakeGame {
    /// The pretend game and the profile that identifies it.
    pub(crate) fn new() -> (Self, ValidProfile<'static>) {
        let text = text();
        let file = image(&text, &block(0x1000, &[(10, RELOCATED as u16), (0, 0)]));
        let profile: &'static Profile = Box::leak(Box::new(Profile {
            identity: Identity {
                disk_sha256: Sha256Digest::of(&file),
                text: TextSection {
                    rva: Rva::new(0x1000),
                    size: text.len() as u32,
                    sha256: Sha256Digest::of(&text),
                },
            },
            ..SYNTHETIC
        }));
        let fake = Fake::new(4242);
        fake.map(BASE, PAGE, Protection::new(0x02), 0);
        fake.poke(BASE, &file[..0x400]);
        fake.poke(BASE + 0x80 + 4 + 20 + 24, &BASE.to_le_bytes());
        fake.map(BASE + 0x1000, PAGE, Protection::EXECUTE_READ, 0);
        let mut loaded = text;
        let relocated = BASE + 0x2010;
        loaded[RELOCATED..RELOCATED + 8].copy_from_slice(&relocated.to_le_bytes());
        fake.poke(BASE + 0x1000, &loaded);
        fake.map(BASE + 0x2000, PAGE, Protection::new(0x02), 0);
        fake.thread(1, BASE + 0x1800);
        fake.thread(2, BASE + 0x1900);
        let path = PathBuf::from(r"C:\Games\Synthetic\synthetic.exe");
        let info = Info {
            module: RefCell::new(Module {
                path: PathBuf::from(r"c:\games\synthetic\SYNTHETIC.EXE"),
                base: Va::new(BASE),
                size: 0x3000,
            }),
            path,
            file: RefCell::new(file),
            exited: Cell::new(false),
            exit_after_waits: Cell::new(None),
            waits: Cell::new(0),
            word: Cell::new(0xFFFF_FFFF),
            stores: Cell::new(0),
            finished: Cell::new(0),
            controls_live: Cell::new(false),
            stop: Cell::new(false),
            interrupted: Cell::new(false),
            withdrawn: Cell::new(false),
            controls_at: Cell::new(0),
            deny: Cell::new(false),
            blocks: RefCell::new(Vec::new()),
            taken: RefCell::new(BTreeSet::new()),
        };
        let game = Self {
            fake,
            info: Rc::new(info),
            owners: Rc::default(),
        };
        (game, ValidProfile::new(profile).unwrap())
    }

    /// The bytes of the loaded code section.
    pub(crate) fn code(&self) -> Vec<u8> {
        self.fake.peek(BASE + 0x1000, 0x1000)
    }

    pub(crate) fn blocks(&self) -> Vec<Block> {
        self.info
            .blocks
            .borrow()
            .iter()
            .map(|&(_, block)| block)
            .collect()
    }

    /// The same game seen by a second trainer.
    pub(crate) fn second(&self) -> Self {
        self.clone()
    }

    fn fail(&self, call: &'static str) -> Result<(), OsError> {
        match self.fake.enter(call) {
            Some(_) => Err(OsError::new(call, 5)),
            None => Ok(()),
        }
    }
}

impl Process for FakeGame {
    fn pid(&self) -> u32 {
        self.fake.pid()
    }
    fn read(&self, at: Va, into: &mut [u8]) -> Result<(), OsError> {
        self.fake.read(at, into)
    }
    fn query(&self, at: Va) -> Result<Region, OsError> {
        self.fake.query(at)
    }
    fn protect(&self, at: Va, len: u64, to: Protection) -> Result<Protection, OsError> {
        self.fake.protect(at, len, to)
    }
    fn write(&self, at: Va, bytes: &[u8]) -> Result<(), OsError> {
        self.fake.write(at, bytes)
    }
    fn flush(&self, at: Va, len: u64) -> Result<(), OsError> {
        self.fake.flush(at, len)
    }
    fn threads(&self) -> Result<Vec<u32>, OsError> {
        self.fake.threads()
    }
    fn open_thread(&self, id: u32) -> Result<Box<dyn Thread>, OsError> {
        self.fake.open_thread(id)
    }
}

impl Game for FakeGame {
    fn image_path(&self) -> Result<PathBuf, OsError> {
        self.fail("image_path")?;
        Ok(self.info.path.clone())
    }

    fn read_executable(&self, path: &Path, limit: u64) -> Result<Vec<u8>, OsError> {
        self.fail("read_executable")?;
        assert_eq!(path, self.info.path);
        let file = self.info.file.borrow();
        if file.len() as u64 > limit {
            return Err(OsError::new("ReadFile", 8));
        }
        Ok(file.clone())
    }

    fn main_module(&self) -> Result<Module, OsError> {
        self.fail("main_module")?;
        Ok(self.info.module.borrow().clone())
    }

    fn exited(&self) -> bool {
        self.info.exited.get()
    }

    fn wait(&self, _timeout: Duration) {
        let waits = self.info.waits.get() + 1;
        self.info.waits.set(waits);
        if self.info.exit_after_waits.get() == Some(waits) {
            self.info.exited.set(true);
        }
    }

    fn granularity(&self) -> u64 {
        0x1_0000
    }

    fn highest_address(&self) -> u64 {
        (1 << 47) - 1
    }

    fn interrupted(&self) -> bool {
        self.info.interrupted.get()
    }

    fn allocate(&self, at: Va, len: u64) -> Result<Box<dyn Allocation>, OsError> {
        if self.fake.enter("allocate").is_some() {
            // An injected failure is a lost race: someone else took the base.
            return Err(OsError::new("VirtualAllocEx", 487));
        }
        if self.info.deny.get() {
            return Err(OsError::new("VirtualAllocEx", 5));
        }
        if self.info.taken.borrow().contains(&at.get()) || self.fake.occupied(at.get(), len) {
            return Err(OsError::new("VirtualAllocEx", 487));
        }
        self.fake.map(at.get(), len, Protection::READ_WRITE, 0);
        let mut blocks = self.info.blocks.borrow_mut();
        blocks.push((at.get(), Block::Live));
        Ok(Box::new(FakeBlock {
            game: self.clone(),
            index: blocks.len() - 1,
            base: at,
            len,
        }))
    }

    fn controls(&self, at: Va) -> Result<Box<dyn Controls>, OsError> {
        self.fail("controls")?;
        if self.info.taken.borrow().contains(&at.get()) || self.fake.occupied(at.get(), PAGE) {
            return Err(OsError::new("MapViewOfFileNuma2", 487));
        }
        self.info.controls_at.set(at.get());
        self.fake.map(at.get(), PAGE, Protection::READ_WRITE, 0);
        self.info.word.set(0);
        self.info.controls_live.set(true);
        Ok(Box::new(FakeControls(Rc::clone(&self.info))))
    }

    fn claim(&self, _slug: ModSlug) -> Result<Box<dyn Owner>, ClaimError> {
        self.fail("claim").map_err(ClaimError::Os)?;
        let pid = self.pid();
        if !self.owners.borrow_mut().insert(pid) {
            return Err(ClaimError::Owned);
        }
        Ok(Box::new(FakeOwner {
            game: self.clone(),
            pid,
        }))
    }
}

struct FakeBlock {
    game: FakeGame,
    index: usize,
    base: Va,
    len: u64,
}

impl Allocation for FakeBlock {
    fn base(&self) -> Va {
        self.base
    }

    fn retain(self: Box<Self>) {
        self.game.info.blocks.borrow_mut()[self.index].1 = Block::Retained;
        std::mem::forget(self);
    }
}

impl Drop for FakeBlock {
    fn drop(&mut self) {
        self.game.fake.unmap(self.base.get(), self.len);
        self.game.info.blocks.borrow_mut()[self.index].1 = Block::Freed;
    }
}

struct FakeControls(Rc<Info>);

impl Controls for FakeControls {
    fn remote(&self) -> Va {
        Va::new(self.0.controls_at.get())
    }

    fn load(&self) -> u32 {
        self.0.word.get()
    }

    fn withdraw(&self) {
        self.0.withdrawn.set(true);
    }

    fn store(&self, word: u32) {
        let info = &self.0;
        info.word.set(word);
        info.stores.set(info.stores.get() + 1);
    }
}

impl Drop for FakeControls {
    fn drop(&mut self) {
        let info = &self.0;
        info.finished.set(info.finished.get() + 1);
        info.word.set(0);
        info.controls_live.set(false);
    }
}

struct FakeOwner {
    game: FakeGame,
    pid: u32,
}

impl Owner for FakeOwner {
    fn stop_requested(&self) -> bool {
        self.game.info.stop.get()
    }
}

impl Drop for FakeOwner {
    fn drop(&mut self) {
        self.game.owners.borrow_mut().remove(&self.pid);
    }
}
