// SPDX-License-Identifier: AGPL-3.0-only
//! The control word: one page of a pagefile-backed section, mapped into the trainer and into the game.
//!
//! The game's view is made with `MapViewOfFileNuma2`, which Windows 10 version 1703 introduced. It is looked
//! up at run time, first in `kernelbase.dll` then in `kernel32.dll`, so the trainer never imports it; without
//! it the trainer refuses rather than falling back to anything weaker. Both views see the same physical page,
//! so one interlocked store on the trainer's view is what the game's caves read.
//!
//! The game's view is never unmapped: a cave may read it after the trainer has gone. The trainer's view is
//! zeroed and unmapped when the mapping drops, after the shutdown bridge has stopped every callback from
//! using it.

use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle as _, OwnedHandle};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};

use windows_sys::Win32::Foundation::{
    ERROR_PROC_NOT_FOUND, FALSE, HANDLE, INVALID_HANDLE_VALUE, TRUE,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    PAGE_READWRITE, UnmapViewOfFile,
};
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};

use crate::addr::Va;
use crate::patch::OsError;
use crate::session::Controls;

use super::bridge::{self, Lease};
use super::process::none;
use super::{last_error, owned, wide};

/// One page.
const SIZE: usize = 4096;
/// `NUMA_NO_PREFERRED_NODE`.
const NO_PREFERRED_NODE: u32 = 0xFFFF_FFFF;

/// `MapViewOfFileNuma2`.
type MapRemote = unsafe extern "system" fn(
    section: HANDLE,
    process: HANDLE,
    offset: u64,
    base: *const c_void,
    size: usize,
    allocation: u32,
    protection: u32,
    node: u32,
) -> *mut c_void;

/// Finds `MapViewOfFileNuma2`, or explains that this Windows is too old.
fn map_remote() -> Result<MapRemote, OsError> {
    for module in ["kernelbase.dll", "kernel32.dll"] {
        let name = wide(module);
        // SAFETY: `name` is a NUL-terminated UTF-16 string; both modules are loaded in every process.
        let handle = unsafe { GetModuleHandleW(name.as_ptr()) };
        if handle.is_null() {
            continue;
        }
        // SAFETY: `handle` is a loaded module and the name is a NUL-terminated ANSI string.
        let found = unsafe { GetProcAddress(handle, c"MapViewOfFileNuma2".as_ptr().cast()) };
        if let Some(found) = found {
            // SAFETY: the export has this documented signature; both are `extern "system"` function pointers
            // of the same size.
            return Ok(unsafe {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, MapRemote>(found)
            });
        }
    }
    Err(OsError::new("MapViewOfFileNuma2", ERROR_PROC_NOT_FOUND))
}

/// The control word, shared with one game.
pub(crate) struct Mapping {
    _section: OwnedHandle,
    word: NonNull<AtomicU32>,
    remote: Va,
    lease: Box<Lease>,
    registered: bool,
}

impl Mapping {
    /// Maps a zeroed control word into the trainer and into `process`.
    pub(crate) fn new(process: HANDLE) -> Result<Self, OsError> {
        let map = map_remote()?;
        // SAFETY: a pagefile-backed section takes no pointers but the null attributes and name.
        let raw = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                none(),
                PAGE_READWRITE,
                0,
                SIZE as u32,
                none(),
            )
        };
        let section = owned(raw, "CreateFileMappingW")?;
        // SAFETY: `section` is a live read-write section of one page.
        let view = unsafe {
            MapViewOfFile(
                section.as_raw_handle(),
                FILE_MAP_READ | FILE_MAP_WRITE,
                0,
                0,
                SIZE,
            )
        };
        let word = NonNull::new(view.Value.cast::<AtomicU32>())
            .ok_or_else(|| last_error("MapViewOfFile"))?;
        let unmap = || {
            // SAFETY: `view` is the trainer's own view, mapped above and not yet unmapped.
            let _unmapped = unsafe { UnmapViewOfFile(view) };
        };
        // SAFETY: the view is page-aligned, so 4-byte aligned, and stays mapped for this function.
        unsafe { word.as_ref() }.store(0, Ordering::SeqCst);
        // SAFETY: `map` is `MapViewOfFileNuma2`; `section` and `process` are live handles.
        let remote = unsafe {
            map(
                section.as_raw_handle(),
                process,
                0,
                none(),
                SIZE,
                0,
                PAGE_READWRITE,
                NO_PREFERRED_NODE,
            )
        };
        if remote.is_null() {
            let error = last_error("MapViewOfFileNuma2");
            unmap();
            return Err(error);
        }
        // SAFETY: a manual-reset, unsignalled, unnamed event takes no pointers but the null ones.
        let raw = unsafe { CreateEventW(none(), TRUE, FALSE, none()) };
        let completion = match owned(raw, "CreateEventW") {
            Ok(event) => event,
            Err(error) => {
                unmap();
                return Err(error);
            }
        };
        let lease = Box::new(Lease::new(word, completion));
        let registered = bridge::publish(&lease);
        Ok(Self {
            _section: section,
            word,
            remote: Va::new(remote as usize as u64),
            lease,
            registered,
        })
    }

    fn word(&self) -> &AtomicU32 {
        // SAFETY: the view stays mapped until `drop`, and is page-aligned.
        unsafe { self.word.as_ref() }
    }
}

impl Controls for Mapping {
    fn remote(&self) -> Va {
        self.remote
    }

    fn store(&self, word: u32) {
        self.word().store(word, Ordering::SeqCst);
    }

    fn finished(&self) {
        // SAFETY: the completion event is a live event handle owned by the lease.
        let _set = unsafe { SetEvent(self.lease.completion()) };
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        self.word().store(0, Ordering::SeqCst);
        if self.registered {
            bridge::retire(&self.lease);
        }
        let view = MEMORY_MAPPED_VIEW_ADDRESS {
            Value: self.word.as_ptr().cast(),
        };
        // SAFETY: no callback can reach the view any more, and this is the trainer's own mapping of it.
        let _unmapped = unsafe { UnmapViewOfFile(view) };
    }
}
