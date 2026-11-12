// SPDX-License-Identifier: AGPL-3.0-or-later
//! A pretend game process for tests: pages with protections, threads with positions, and a fault plan that
//! fails, half-performs or panics any call on any occurrence.
#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;

use crate::addr::Va;

use super::{OsError, Process, Protection, Region, RegionState, Thread};

pub(crate) const PAGE: u64 = 0x1000;

/// What an injected fault does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    /// The call fails and changes nothing.
    Fail,
    /// A write changes only its first `n` bytes, then fails.
    Partial(usize),
    /// The call panics before doing anything.
    Panic,
}

#[derive(Clone, Debug)]
struct Page {
    bytes: Vec<u8>,
    protect: Protection,
}

#[derive(Clone, Debug)]
pub(crate) struct FakeThread {
    pub(crate) owner: u32,
    pub(crate) rip: Va,
    pub(crate) suspended: u32,
}

#[derive(Debug, Default)]
pub(crate) struct State {
    pid: u32,
    pages: BTreeMap<u64, Page>,
    reserved: BTreeMap<u64, u64>,
    pub(crate) threads: BTreeMap<u32, FakeThread>,
    /// Positions each thread moves through, one per context read, before staying at the last.
    pub(crate) moves: BTreeMap<u32, VecDeque<Va>>,
    /// Thread lists to report, one per enumeration, before falling back to the live list.
    pub(crate) snapshots: VecDeque<Vec<u32>>,
    faults: HashMap<(&'static str, usize), Fault>,
    calls: HashMap<&'static str, usize>,
    /// Every call made, in order.
    pub(crate) log: Vec<&'static str>,
}

/// A shared handle on the pretend process.
#[derive(Clone, Debug, Default)]
pub(crate) struct Fake(pub(crate) Rc<RefCell<State>>);

impl Fake {
    pub(crate) fn new(pid: u32) -> Self {
        let fake = Self::default();
        fake.0.borrow_mut().pid = pid;
        fake
    }

    /// Commits `[at, at + len)` with `protect`, filled with `fill`.
    pub(crate) fn map(&self, at: u64, len: u64, protect: Protection, fill: u8) {
        let mut state = self.0.borrow_mut();
        let mut page = at & !(PAGE - 1);
        while page < at + len {
            state.pages.insert(
                page,
                Page {
                    bytes: vec![fill; PAGE as usize],
                    protect,
                },
            );
            page += PAGE;
        }
    }

    /// Removes the pages of `[at, at + len)`.
    pub(crate) fn unmap(&self, at: u64, len: u64) {
        let mut state = self.0.borrow_mut();
        let mut page = at & !(PAGE - 1);
        while page < at + len {
            state.pages.remove(&page);
            page += PAGE;
        }
    }

    /// Whether any page of `[at, at + len)` is mapped or reserved.
    pub(crate) fn occupied(&self, at: u64, len: u64) -> bool {
        let state = self.0.borrow();
        let end = at + len;
        state.pages.range(at & !(PAGE - 1)..end).next().is_some()
            || state
                .reserved
                .iter()
                .any(|(&start, &size)| start < end && at < start + size)
    }

    pub(crate) fn poke(&self, at: u64, bytes: &[u8]) {
        let mut state = self.0.borrow_mut();
        for (i, &byte) in bytes.iter().enumerate() {
            let address = at + i as u64;
            let page = state
                .pages
                .get_mut(&(address & !(PAGE - 1)))
                .expect("poke into mapped memory");
            page.bytes[(address & (PAGE - 1)) as usize] = byte;
        }
    }

    pub(crate) fn peek(&self, at: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0; len];
        self.0
            .borrow()
            .copy_out(at, &mut out)
            .expect("peek mapped memory");
        out
    }

    pub(crate) fn protection(&self, at: u64) -> Protection {
        self.0.borrow().pages[&(at & !(PAGE - 1))].protect
    }

    pub(crate) fn thread(&self, id: u32, rip: u64) {
        let pid = self.0.borrow().pid;
        self.0.borrow_mut().threads.insert(
            id,
            FakeThread {
                owner: pid,
                rip: Va::new(rip),
                suspended: 0,
            },
        );
    }

    pub(crate) fn suspend_counts(&self) -> Vec<u32> {
        self.0
            .borrow()
            .threads
            .values()
            .map(|thread| thread.suspended)
            .collect()
    }

    /// Makes the `nth` call (counting from 0) to `call` do `fault`.
    pub(crate) fn fault(&self, call: &'static str, nth: usize, fault: Fault) {
        self.0.borrow_mut().faults.insert((call, nth), fault);
    }

    /// Makes the `nth` call to `call` from now (counting from 0) do `fault`.
    pub(crate) fn fault_next(&self, call: &'static str, nth: usize, fault: Fault) {
        let done = self.calls(call);
        self.fault(call, done + nth, fault);
    }

    pub(crate) fn clear_faults(&self) {
        self.0.borrow_mut().faults.clear();
    }

    /// How often `call` was made.
    pub(crate) fn calls(&self, call: &'static str) -> usize {
        self.0.borrow().calls.get(call).copied().unwrap_or(0)
    }

    /// The total number of calls made.
    pub(crate) fn total_calls(&self) -> usize {
        self.0.borrow().log.len()
    }

    /// Counts a call to `call` and returns the fault planned for it.
    pub(crate) fn enter(&self, call: &'static str) -> Option<Fault> {
        let fault = {
            let mut state = self.0.borrow_mut();
            let count = state.calls.entry(call).or_default();
            let nth = *count;
            *count += 1;
            state.log.push(call);
            state.faults.get(&(call, nth)).copied()
        };
        if fault == Some(Fault::Panic) {
            panic!("injected panic in {call}");
        }
        fault
    }
}

impl State {
    fn copy_out(&self, at: u64, out: &mut [u8]) -> Option<()> {
        for (i, byte) in out.iter_mut().enumerate() {
            let address = at.checked_add(i as u64)?;
            let page = self.pages.get(&(address & !(PAGE - 1)))?;
            if !page.protect.accessible() {
                return None;
            }
            *byte = page.bytes[(address & (PAGE - 1)) as usize];
        }
        Some(())
    }

    fn writable(&self, at: u64, len: usize) -> bool {
        (0..len as u64).all(|i| {
            self.pages
                .get(&((at + i) & !(PAGE - 1)))
                .is_some_and(|page| {
                    matches!(
                        page.protect,
                        Protection::READ_WRITE | Protection::EXECUTE_READ_WRITE
                    )
                })
        })
    }

    fn store(&mut self, at: u64, bytes: &[u8]) {
        for (i, &byte) in bytes.iter().enumerate() {
            let address = at + i as u64;
            if let Some(page) = self.pages.get_mut(&(address & !(PAGE - 1))) {
                page.bytes[(address & (PAGE - 1)) as usize] = byte;
            }
        }
    }

    fn region(&self, at: u64) -> Region {
        let page = at & !(PAGE - 1);
        if let Some(first) = self.pages.get(&page) {
            let mut base = page;
            while base >= PAGE
                && self
                    .pages
                    .get(&(base - PAGE))
                    .is_some_and(|p| p.protect == first.protect)
            {
                base -= PAGE;
            }
            let mut end = page + PAGE;
            while self
                .pages
                .get(&end)
                .is_some_and(|p| p.protect == first.protect)
            {
                end += PAGE;
            }
            return Region {
                base: Va::new(base),
                size: end - base,
                state: RegionState::Committed,
                protect: first.protect,
            };
        }
        if let Some((&start, &len)) = self.reserved.range(..=at).next_back()
            && at < start + len
        {
            return Region {
                base: Va::new(start),
                size: len,
                state: RegionState::Reserved,
                protect: Protection::new(0),
            };
        }
        let below = self
            .pages
            .range(..page)
            .next_back()
            .map(|(&p, _)| p + PAGE)
            .into_iter()
            .chain(self.reserved.range(..page).map(|(&s, &l)| s + l))
            .filter(|&end| end <= page)
            .max()
            .unwrap_or(0);
        let above = self
            .pages
            .range(page..)
            .next()
            .map(|(&p, _)| p)
            .into_iter()
            .chain(self.reserved.range(page..).map(|(&s, _)| s))
            .min()
            .unwrap_or(1 << 47);
        Region {
            base: Va::new(below),
            size: above - below,
            state: RegionState::Free,
            protect: Protection::new(0),
        }
    }
}

fn error(call: &'static str) -> OsError {
    OsError::new(call, 5)
}

impl Process for Fake {
    fn pid(&self) -> u32 {
        self.0.borrow().pid
    }

    fn read(&self, at: Va, into: &mut [u8]) -> Result<(), OsError> {
        if self.enter("read").is_some() {
            return Err(error("ReadProcessMemory"));
        }
        self.0
            .borrow()
            .copy_out(at.get(), into)
            .ok_or(error("ReadProcessMemory"))
    }

    fn query(&self, at: Va) -> Result<Region, OsError> {
        if self.enter("query").is_some() {
            return Err(error("VirtualQueryEx"));
        }
        Ok(self.0.borrow().region(at.get()))
    }

    fn protect(&self, at: Va, len: u64, to: Protection) -> Result<Protection, OsError> {
        if self.enter("protect").is_some() {
            return Err(error("VirtualProtectEx"));
        }
        let mut state = self.0.borrow_mut();
        let first = at.get() & !(PAGE - 1);
        let mut page = first;
        while page < at.get() + len {
            if !state.pages.contains_key(&page) {
                return Err(error("VirtualProtectEx"));
            }
            page += PAGE;
        }
        let previous = state.pages[&first].protect;
        let mut page = first;
        while page < at.get() + len {
            if let Some(entry) = state.pages.get_mut(&page) {
                entry.protect = to;
            }
            page += PAGE;
        }
        Ok(previous)
    }

    fn write(&self, at: Va, bytes: &[u8]) -> Result<(), OsError> {
        let fault = self.enter("write");
        let mut state = self.0.borrow_mut();
        if !state.writable(at.get(), bytes.len()) {
            return Err(error("WriteProcessMemory"));
        }
        match fault {
            Some(Fault::Partial(n)) => {
                state.store(at.get(), &bytes[..n.min(bytes.len())]);
                Err(error("WriteProcessMemory"))
            }
            Some(_) => Err(error("WriteProcessMemory")),
            None => {
                state.store(at.get(), bytes);
                Ok(())
            }
        }
    }

    fn flush(&self, _at: Va, _len: u64) -> Result<(), OsError> {
        match self.enter("flush") {
            Some(_) => Err(error("FlushInstructionCache")),
            None => Ok(()),
        }
    }

    fn threads(&self) -> Result<Vec<u32>, OsError> {
        if self.enter("threads").is_some() {
            return Err(error("Thread32Next"));
        }
        let mut state = self.0.borrow_mut();
        Ok(match state.snapshots.pop_front() {
            Some(ids) => ids,
            None => state.threads.keys().copied().collect(),
        })
    }

    fn open_thread(&self, id: u32) -> Result<Box<dyn Thread>, OsError> {
        if self.enter("open_thread").is_some() || !self.0.borrow().threads.contains_key(&id) {
            return Err(error("OpenThread"));
        }
        Ok(Box::new(Handle {
            fake: self.clone(),
            id,
        }))
    }
}

struct Handle {
    fake: Fake,
    id: u32,
}

impl Handle {
    fn with<T>(
        &self,
        call: &'static str,
        f: impl FnOnce(&mut FakeThread) -> T,
    ) -> Result<T, OsError> {
        if self.fake.enter(call).is_some() {
            return Err(error(call));
        }
        let mut state = self.fake.0.borrow_mut();
        state.threads.get_mut(&self.id).map(f).ok_or(error(call))
    }
}

impl Thread for Handle {
    fn process_id(&self) -> Result<u32, OsError> {
        self.with("thread_owner", |thread| thread.owner)
    }

    fn suspend(&self) -> Result<u32, OsError> {
        self.with("suspend", |thread| {
            thread.suspended += 1;
            thread.suspended - 1
        })
    }

    fn resume(&self) -> Result<u32, OsError> {
        self.with("resume", |thread| {
            thread.suspended = thread.suspended.saturating_sub(1);
            thread.suspended + 1
        })
    }

    fn rip(&self) -> Result<Va, OsError> {
        let next = self
            .fake
            .0
            .borrow_mut()
            .moves
            .get_mut(&self.id)
            .and_then(VecDeque::pop_front);
        self.with("context", |thread| {
            if let Some(rip) = next {
                thread.rip = rip;
            }
            thread.rip
        })
    }
}
