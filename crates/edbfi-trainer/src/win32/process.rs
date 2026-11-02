// SPDX-License-Identifier: AGPL-3.0-only
//! A game process opened by handle: discovery, memory, threads and allocation.

use std::ffi::c_void;
use std::fs::OpenOptions;
use std::io::Read as _;
use std::mem::{size_of, zeroed};
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::{AsRawHandle as _, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use std::time::Duration;

use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, FALSE, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    CONTEXT, CONTEXT_CONTROL_AMD64, FlushInstructionCache, GetThreadContext, ReadProcessMemory,
    WriteProcessMemory,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, PROCESSENTRY32W, Process32FirstW,
    Process32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32, TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD,
    THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_FREE, MEM_RELEASE, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_READWRITE,
    VirtualAllocEx, VirtualFreeEx, VirtualProtectEx, VirtualQueryEx,
};
use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
use windows_sys::Win32::System::Threading::{
    GetProcessIdOfThread, OpenProcess, OpenThread, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION,
    PROCESS_VM_READ, PROCESS_VM_WRITE, QueryFullProcessImageNameW, ResumeThread, SuspendThread,
    THREAD_GET_CONTEXT, THREAD_QUERY_INFORMATION, THREAD_SUSPEND_RESUME, WaitForSingleObject,
};

use crate::addr::Va;
use crate::patch::{OsError, Process, Protection, Region, RegionState, Thread};
use crate::profile::ModSlug;
use crate::session::{Allocation, ClaimError, Controls, Game, Module, Owner};

use super::{from_wide, last_code, last_error, millis, owned, owned_or_invalid};

/// The ids of every process whose executable is named `exe`, ignoring ASCII case.
pub(crate) fn find(exe: &str) -> Result<Vec<u32>, OsError> {
    // SAFETY: a process snapshot takes no pointers.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    let snapshot = owned_or_invalid(raw, "CreateToolhelp32Snapshot")?;
    // SAFETY: `PROCESSENTRY32W` is plain data, valid when zeroed.
    let mut entry: PROCESSENTRY32W = unsafe { zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut found = Vec::new();
    // SAFETY: `snapshot` is a live process snapshot and `entry` a sized, writable entry.
    let mut more = unsafe { Process32FirstW(snapshot.as_raw_handle(), &raw mut entry) } != FALSE;
    while more {
        if from_wide(&entry.szExeFile)
            .to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case(exe))
        {
            found.push(entry.th32ProcessID);
        }
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot.as_raw_handle(), &raw mut entry) } != FALSE;
    }
    if last_code() != ERROR_NO_MORE_FILES {
        return Err(last_error("Process32NextW"));
    }
    Ok(found)
}

/// A game process, open by handle. The handle pins the process: its id cannot be reused while it is open.
pub(crate) struct WinGame {
    handle: Arc<OwnedHandle>,
    pid: u32,
}

impl WinGame {
    /// Opens process `pid` to identify it, and with `write`, to patch it.
    pub(crate) fn open(pid: u32, write: bool) -> Result<Self, OsError> {
        let mut access = PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | SYNCHRONIZE;
        if write {
            access |= PROCESS_VM_WRITE | PROCESS_VM_OPERATION;
        }
        // SAFETY: `OpenProcess` takes no pointers.
        let raw = unsafe { OpenProcess(access, FALSE, pid) };
        Ok(Self {
            handle: Arc::new(owned(raw, "OpenProcess")?),
            pid,
        })
    }

    fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle()
    }
}

/// A thread of the game, open to suspend, resume and inspect.
struct WinThread(OwnedHandle);

/// A `CONTEXT` aligned as `GetThreadContext` requires; `windows-sys` declares it with 8-byte alignment.
#[repr(C, align(16))]
struct AlignedContext(CONTEXT);

impl Thread for WinThread {
    fn process_id(&self) -> Result<u32, OsError> {
        // SAFETY: the handle is a live thread handle with query rights.
        match unsafe { GetProcessIdOfThread(self.0.as_raw_handle()) } {
            0 => Err(last_error("GetProcessIdOfThread")),
            pid => Ok(pid),
        }
    }

    fn suspend(&self) -> Result<u32, OsError> {
        // SAFETY: the handle is a live thread handle with suspend rights.
        match unsafe { SuspendThread(self.0.as_raw_handle()) } {
            u32::MAX => Err(last_error("SuspendThread")),
            previous => Ok(previous),
        }
    }

    fn resume(&self) -> Result<u32, OsError> {
        // SAFETY: the handle is a live thread handle with resume rights.
        match unsafe { ResumeThread(self.0.as_raw_handle()) } {
            u32::MAX => Err(last_error("ResumeThread")),
            previous => Ok(previous),
        }
    }

    fn rip(&self) -> Result<Va, OsError> {
        // SAFETY: `CONTEXT` is plain data, valid when zeroed.
        let mut context = AlignedContext(unsafe { zeroed() });
        context.0.ContextFlags = CONTEXT_CONTROL_AMD64;
        // SAFETY: the handle is a live, suspended thread handle with context rights, and `context` is a
        // writable, 16-byte aligned `CONTEXT` with its flags set.
        if unsafe { GetThreadContext(self.0.as_raw_handle(), &raw mut context.0) } == FALSE {
            return Err(last_error("GetThreadContext"));
        }
        Ok(Va::new(context.0.Rip))
    }
}

fn address(at: Va) -> *const c_void {
    at.get() as usize as *const c_void
}

impl Process for WinGame {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn read(&self, at: Va, into: &mut [u8]) -> Result<(), OsError> {
        let mut done = 0;
        // SAFETY: `into` is writable for its length, and `done` is a writable count. The remote address is only
        // read by the kernel, which checks it.
        let ok = unsafe {
            ReadProcessMemory(
                self.raw(),
                address(at),
                into.as_mut_ptr().cast(),
                into.len(),
                &raw mut done,
            )
        };
        if ok == FALSE {
            return Err(last_error("ReadProcessMemory"));
        }
        if done != into.len() {
            return Err(OsError::new("ReadProcessMemory", 299));
        }
        Ok(())
    }

    fn query(&self, at: Va) -> Result<Region, OsError> {
        // SAFETY: plain data, valid when zeroed.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { zeroed() };
        // SAFETY: `info` is writable for the length passed.
        let written = unsafe {
            VirtualQueryEx(
                self.raw(),
                address(at),
                &raw mut info,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if written == 0 {
            return Err(last_error("VirtualQueryEx"));
        }
        let state = match info.State {
            MEM_COMMIT => RegionState::Committed,
            MEM_FREE => RegionState::Free,
            _ => RegionState::Reserved,
        };
        Ok(Region {
            base: Va::new(info.BaseAddress as usize as u64),
            size: info.RegionSize as u64,
            state,
            protect: Protection::new(info.Protect),
        })
    }

    fn protect(&self, at: Va, len: u64, to: Protection) -> Result<Protection, OsError> {
        let mut previous = 0;
        // SAFETY: `previous` is writable; the remote range is checked by the kernel.
        let ok = unsafe {
            VirtualProtectEx(
                self.raw(),
                address(at),
                len as usize,
                to.get(),
                &raw mut previous,
            )
        };
        if ok == FALSE {
            return Err(last_error("VirtualProtectEx"));
        }
        Ok(Protection::new(previous))
    }

    fn write(&self, at: Va, bytes: &[u8]) -> Result<(), OsError> {
        let mut done = 0;
        // SAFETY: `bytes` is readable for its length and `done` writable; the kernel checks the remote range.
        let ok = unsafe {
            WriteProcessMemory(
                self.raw(),
                address(at),
                bytes.as_ptr().cast(),
                bytes.len(),
                &raw mut done,
            )
        };
        if ok == FALSE {
            return Err(last_error("WriteProcessMemory"));
        }
        if done != bytes.len() {
            return Err(OsError::new("WriteProcessMemory", 299));
        }
        Ok(())
    }

    fn flush(&self, at: Va, len: u64) -> Result<(), OsError> {
        // SAFETY: the process handle is live; the remote range is only named, never dereferenced here.
        if unsafe { FlushInstructionCache(self.raw(), address(at), len as usize) } == FALSE {
            return Err(last_error("FlushInstructionCache"));
        }
        Ok(())
    }

    fn threads(&self) -> Result<Vec<u32>, OsError> {
        // SAFETY: a thread snapshot takes no pointers.
        let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        let snapshot = owned_or_invalid(raw, "CreateToolhelp32Snapshot")?;
        // SAFETY: plain data, valid when zeroed.
        let mut entry: THREADENTRY32 = unsafe { zeroed() };
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        let mut ids = Vec::new();
        // SAFETY: `snapshot` is a live thread snapshot and `entry` a sized, writable entry.
        let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &raw mut entry) } != FALSE;
        while more {
            if entry.th32OwnerProcessID == self.pid {
                ids.push(entry.th32ThreadID);
            }
            // SAFETY: as above.
            more = unsafe { Thread32Next(snapshot.as_raw_handle(), &raw mut entry) } != FALSE;
        }
        // An enumeration is complete only if it ran out of entries.
        if last_code() != ERROR_NO_MORE_FILES {
            return Err(last_error("Thread32Next"));
        }
        Ok(ids)
    }

    fn open_thread(&self, id: u32) -> Result<Box<dyn Thread>, OsError> {
        let access = THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION;
        // SAFETY: `OpenThread` takes no pointers.
        let raw = unsafe { OpenThread(access, FALSE, id) };
        Ok(Box::new(WinThread(owned(raw, "OpenThread")?)))
    }
}

/// A block allocated in the game, freed on drop unless retained.
struct WinBlock {
    process: Arc<OwnedHandle>,
    base: Va,
    retained: bool,
}

impl Allocation for WinBlock {
    fn base(&self) -> Va {
        self.base
    }

    fn retain(mut self: Box<Self>) {
        self.retained = true;
    }
}

impl Drop for WinBlock {
    fn drop(&mut self) {
        if !self.retained {
            // SAFETY: `base` is the start of an allocation this block made in the process, which no hook
            // can reach because it was never retained.
            let _freed = unsafe {
                VirtualFreeEx(
                    self.process.as_raw_handle(),
                    self.base.get() as usize as *mut c_void,
                    0,
                    MEM_RELEASE,
                )
            };
        }
    }
}

impl Game for WinGame {
    fn image_path(&self) -> Result<PathBuf, OsError> {
        let mut buffer = vec![0u16; 32_768];
        let mut len = buffer.len() as u32;
        // SAFETY: `buffer` is writable for `len` UTF-16 units and `len` is writable.
        let ok =
            unsafe { QueryFullProcessImageNameW(self.raw(), 0, buffer.as_mut_ptr(), &raw mut len) };
        if ok == FALSE {
            return Err(last_error("QueryFullProcessImageNameW"));
        }
        Ok(PathBuf::from(from_wide(&buffer[..len as usize])))
    }

    fn read_executable(&self, path: &Path, limit: u64) -> Result<Vec<u8>, OsError> {
        let os = |error: std::io::Error| {
            OsError::new(
                "ReadFile",
                error.raw_os_error().map_or(0, |code| code as u32),
            )
        };
        let mut file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(path)
            .map_err(os)?;
        let len = file.metadata().map_err(os)?.len();
        if len == 0 || len > limit {
            return Err(OsError::new("ReadFile", 223));
        }
        let mut bytes = Vec::with_capacity(len as usize);
        file.by_ref()
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(os)?;
        if bytes.len() as u64 > limit {
            return Err(OsError::new("ReadFile", 223));
        }
        Ok(bytes)
    }

    fn main_module(&self) -> Result<Module, OsError> {
        // SAFETY: a module snapshot takes no pointers.
        let raw =
            unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, self.pid) };
        let snapshot = owned_or_invalid(raw, "CreateToolhelp32Snapshot")?;
        // SAFETY: plain data, valid when zeroed.
        let mut entry: MODULEENTRY32W = unsafe { zeroed() };
        entry.dwSize = size_of::<MODULEENTRY32W>() as u32;
        // SAFETY: `snapshot` is a live module snapshot and `entry` a sized, writable entry.
        if unsafe { Module32FirstW(snapshot.as_raw_handle(), &raw mut entry) } == FALSE {
            return Err(last_error("Module32FirstW"));
        }
        Ok(Module {
            path: PathBuf::from(from_wide(&entry.szExePath)),
            base: Va::new(entry.modBaseAddr as usize as u64),
            size: entry.modBaseSize,
        })
    }

    fn exited(&self) -> bool {
        // SAFETY: the handle is a live process handle with `SYNCHRONIZE`.
        unsafe { WaitForSingleObject(self.raw(), 0) == WAIT_OBJECT_0 }
    }

    fn wait(&self, timeout: Duration) {
        // SAFETY: as in `exited`.
        let _signalled = unsafe { WaitForSingleObject(self.raw(), millis(timeout)) };
    }

    fn granularity(&self) -> u64 {
        u64::from(system().dwAllocationGranularity)
    }

    fn highest_address(&self) -> u64 {
        system().lpMaximumApplicationAddress as usize as u64
    }

    fn interrupted(&self) -> bool {
        super::bridge::get().is_some_and(super::bridge::Bridge::requested)
    }

    fn allocate(&self, at: Va, len: u64) -> Result<Box<dyn Allocation>, OsError> {
        // SAFETY: the kernel checks the remote range; nothing local is passed.
        let raw = unsafe {
            VirtualAllocEx(
                self.raw(),
                address(at),
                len as usize,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        if raw.is_null() {
            return Err(last_error("VirtualAllocEx"));
        }
        let block = WinBlock {
            process: Arc::clone(&self.handle),
            base: Va::new(raw as usize as u64),
            retained: false,
        };
        if block.base != at {
            return Err(OsError::new("VirtualAllocEx", 487));
        }
        Ok(Box::new(block))
    }

    fn controls(&self) -> Result<Box<dyn Controls>, OsError> {
        Ok(Box::new(super::controls::Mapping::new(self.raw())?))
    }

    fn claim(&self, slug: ModSlug) -> Result<Box<dyn Owner>, ClaimError> {
        Ok(Box::new(super::owner::claim(slug, self.pid)?))
    }
}

/// This machine's memory layout.
fn system() -> SYSTEM_INFO {
    // SAFETY: plain data, valid when zeroed.
    let mut info: SYSTEM_INFO = unsafe { zeroed() };
    // SAFETY: `info` is writable.
    unsafe { GetSystemInfo(&raw mut info) };
    info
}

/// A null pointer, for optional arguments.
pub(super) const fn none<T>() -> *const T {
    ptr::null()
}
