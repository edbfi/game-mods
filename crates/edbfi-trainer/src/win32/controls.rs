// SPDX-License-Identifier: AGPL-3.0-only
//! The control word: the first four bytes of one read-write page the trainer allocates in the game.
//!
//! The trainer reads and writes the word through the process handle, as it does the caves. Windows does not
//! promise that a 4-byte `WriteProcessMemory` is a single store, but it never needs to be: each cave loads the
//! word once and decides on its own bits alone, and no bit straddles a byte, so a cave racing a write sees
//! each of its bits either before or after it. A torn word can briefly pair bits no selection produces, such
//! as a derived bit without the toggle that implies it, but no cave reads both.
//!
//! Only [`Controls::withdraw`] frees the page, while no cave can read it; it retires the shutdown bridge's
//! lease first, so no callback writes to a page the game may have reused. Once a cave may read the page it
//! stays, even after the trainer has gone: dropping the controls zeroes the word, then retires the lease.

use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle as _, OwnedHandle};
use std::ptr::NonNull;
use std::sync::Arc;

use windows_sys::Win32::Foundation::{
    ERROR_BUSY, ERROR_INVALID_ADDRESS, ERROR_PARTIAL_COPY, FALSE, HANDLE, TRUE,
};
use windows_sys::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx,
};
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};

use crate::addr::Va;
use crate::patch::OsError;
use crate::session::Controls;

use super::bridge::{self, Lease};
use super::process::none;
use super::{last_error, owned};

/// One page.
const SIZE: usize = 4096;

/// Writes `word` at `at` in `process`. Safe from a panic hook or a console handler: no allocation, lock or I/O.
pub(super) fn write_word(process: HANDLE, at: Va, word: u32) -> Result<(), OsError> {
    let bytes = word.to_ne_bytes();
    let mut done = 0;
    // SAFETY: `bytes` is readable for its length and `done` writable; the kernel checks the remote range.
    let written = unsafe {
        WriteProcessMemory(
            process,
            at.get() as usize as *const c_void,
            bytes.as_ptr().cast(),
            bytes.len(),
            &raw mut done,
        )
    };
    if written == FALSE {
        return Err(last_error("WriteProcessMemory"));
    }
    if done != bytes.len() {
        return Err(OsError::new("WriteProcessMemory", ERROR_PARTIAL_COPY));
    }
    Ok(())
}

/// The control word, in a page of one game.
pub(crate) struct Page {
    process: Arc<OwnedHandle>,
    remote: Va,
    lease: NonNull<Lease>,
    /// Whether the bridge's slot holds the lease.
    registered: bool,
    /// Whether [`Controls::withdraw`] freed the page.
    withdrawn: bool,
}

impl Page {
    /// Allocates a zeroed control page in `process` at exactly `at`.
    pub(crate) fn new(process: Arc<OwnedHandle>, at: Va) -> Result<Self, OsError> {
        // SAFETY: a manual-reset, unsignalled, unnamed event takes no pointers but the null ones.
        let raw = unsafe { CreateEventW(none(), TRUE, FALSE, none()) };
        let completion = owned(raw, "CreateEventW")?;
        // Committed pages start zeroed. The page exists before the bridge can reach it, so a callback never
        // writes at an address the game may be using for something else.
        // SAFETY: the kernel checks the remote range; nothing local is passed.
        let raw = unsafe {
            VirtualAllocEx(
                process.as_raw_handle(),
                at.get() as usize as *const c_void,
                SIZE,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        if raw.is_null() {
            return Err(last_error("VirtualAllocEx"));
        }
        let free = || {
            // SAFETY: `raw` is this page, allocated above, which no cave can reach yet.
            let _freed = unsafe { VirtualFreeEx(process.as_raw_handle(), raw, 0, MEM_RELEASE) };
        };
        let remote = Va::new(raw as usize as u64);
        if remote != at {
            free();
            return Err(OsError::new("VirtualAllocEx", ERROR_INVALID_ADDRESS));
        }
        // The lease is owned through a raw pointer from here on, so the pointer the bridge holds stays valid
        // however the page moves; it is freed only after the bridge has no reader of it.
        let lease = NonNull::from(Box::leak(Box::new(Lease::new(
            Arc::clone(&process),
            remote,
            completion,
        ))));
        // With the shutdown bridge installed, every session must be reachable from it, one at a time.
        let registered = bridge::publish(lease);
        if !registered && bridge::get().is_some() {
            // SAFETY: `lease` came from `Box::leak` above and was never published.
            drop(unsafe { Box::from_raw(lease.as_ptr()) });
            free();
            return Err(OsError::new("VirtualAllocEx", ERROR_BUSY));
        }
        Ok(Self {
            process,
            remote,
            lease,
            registered,
            withdrawn: false,
        })
    }

    fn lease(&self) -> &Lease {
        // SAFETY: the lease lives until `drop` frees it.
        unsafe { self.lease.as_ref() }
    }
}

impl Controls for Page {
    fn remote(&self) -> Va {
        self.remote
    }

    fn store(&self, word: u32) -> Result<(), OsError> {
        write_word(self.process.as_raw_handle(), self.remote, word)
    }

    fn load(&self) -> Result<u32, OsError> {
        let mut bytes = [0; 4];
        let mut done = 0;
        // SAFETY: `bytes` is writable for its length and `done` writable; the kernel checks the remote range.
        let read = unsafe {
            ReadProcessMemory(
                self.process.as_raw_handle(),
                self.remote.get() as usize as *const c_void,
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                &raw mut done,
            )
        };
        if read == FALSE {
            return Err(last_error("ReadProcessMemory"));
        }
        if done != bytes.len() {
            return Err(OsError::new("ReadProcessMemory", ERROR_PARTIAL_COPY));
        }
        Ok(u32::from_ne_bytes(bytes))
    }

    fn withdraw(&mut self) {
        if self.withdrawn {
            return;
        }
        // Nothing will be restored: release any close notice waiting on this session, then take the lease out
        // of the bridge, so no callback can write to the page once it is gone.
        // SAFETY: the completion event is a live event handle owned by the lease.
        let _set = unsafe { SetEvent(self.lease().completion()) };
        if self.registered {
            bridge::retire(self.lease);
            self.registered = false;
        }
        self.withdrawn = true;
        // SAFETY: `remote` is this page, which no cave can reach yet; the process handle is live with
        // `PROCESS_VM_OPERATION`.
        let _freed = unsafe {
            VirtualFreeEx(
                self.process.as_raw_handle(),
                self.remote.get() as usize as *mut c_void,
                0,
                MEM_RELEASE,
            )
        };
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        // A freed page may belong to the game again; a game that already exited has no page left to zero.
        if !self.withdrawn {
            let _zeroed = self.store(0);
        }
        // Nothing more will be restored: release any close notice waiting on this session.
        // SAFETY: the completion event is a live event handle owned by the lease.
        let _set = unsafe { SetEvent(self.lease().completion()) };
        if self.registered {
            bridge::retire(self.lease);
        }
        // SAFETY: the lease came from `Box::leak` in `new`, and no callback can reach it any more.
        drop(unsafe { Box::from_raw(self.lease.as_ptr()) });
    }
}
