// SPDX-License-Identifier: AGPL-3.0-or-later
//! The trainer's console output: virtual-terminal processing, UTF-8 and the title, put back on drop.
//!
//! The screen opens `CONOUT$`, so it reaches the console even when standard output is redirected. Only one is
//! open at a time, because the panic hook's reset of the alternate screen is process-wide.

use std::os::windows::io::{AsRawHandle as _, OwnedHandle};
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{
    ERROR_BUSY, FALSE, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    GetConsoleMode, GetConsoleOutputCP, GetConsoleScreenBufferInfo, GetConsoleTitleW, GetStdHandle,
    STD_OUTPUT_HANDLE, SetConsoleMode, SetConsoleOutputCP, SetConsoleTitleW,
};

use crate::patch::OsError;

use super::process::none;
use super::{bridge, clear_last_error, last_code, last_error, owned_or_invalid, wide};

/// The modes the screen turns on; every other bit stays as found.
const ON: u32 = ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING;

/// The UTF-8 code page.
const UTF8: u32 = 65_001;

/// The longest title read back, in UTF-16 units: the console's own limit.
const TITLE_LIMIT: usize = 32_768;

/// Whether a screen is open.
static OPEN: AtomicBool = AtomicBool::new(false);

/// The console's output, with virtual-terminal processing and UTF-8 in effect.
pub(crate) struct Screen {
    handle: OwnedHandle,
    mode: u32,
    code_page: u32,
    /// The title found, or `None` if it could not be read, so it is left as the trainer set it.
    title: Option<Vec<u16>>,
}

impl Screen {
    /// Opens the console's output, saves its mode, code page and title, and turns on the screen's modes and
    /// UTF-8.
    pub(crate) fn open() -> Result<Self, OsError> {
        if OPEN.swap(true, Ordering::SeqCst) {
            return Err(OsError::new("CreateFileW", ERROR_BUSY));
        }
        let opened = open_output().and_then(|handle| {
            let mode = mode(handle.as_raw_handle())?;
            // SAFETY: takes no arguments.
            let code_page = unsafe { GetConsoleOutputCP() };
            if code_page == 0 {
                return Err(last_error("GetConsoleOutputCP"));
            }
            Ok(Self {
                handle,
                mode,
                code_page,
                title: title(),
            })
        });
        let screen = match opened {
            Ok(screen) => screen,
            Err(error) => {
                OPEN.store(false, Ordering::SeqCst);
                return Err(error);
            }
        };
        // From here on, dropping the screen puts back what it found.
        bridge::hook();
        // SAFETY: `raw` is a console output handle.
        if unsafe { SetConsoleMode(screen.raw(), screen.mode | ON) } == FALSE {
            return Err(last_error("SetConsoleMode"));
        }
        // SAFETY: takes no pointers.
        if unsafe { SetConsoleOutputCP(UTF8) } == FALSE {
            return Err(last_error("SetConsoleOutputCP"));
        }
        Ok(screen)
    }

    fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle()
    }

    /// The visible window, in cells: width, then height.
    pub(crate) fn size(&self) -> Result<(u16, u16), OsError> {
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        // SAFETY: `raw` is a console output handle and `info` is writable.
        if unsafe { GetConsoleScreenBufferInfo(self.raw(), &raw mut info) } == FALSE {
            return Err(last_error("GetConsoleScreenBufferInfo"));
        }
        let window = info.srWindow;
        let cells =
            |from: i16, to: i16| u16::try_from(i32::from(to) - i32::from(from) + 1).unwrap_or(0);
        Ok((
            cells(window.Left, window.Right),
            cells(window.Top, window.Bottom),
        ))
    }

    /// Sets the console window's title.
    pub(crate) fn set_title(&self, title: &str) -> Result<(), OsError> {
        set_title(&wide(title))
    }

    /// Has a panic leave the alternate screen and restore the output mode found.
    pub(crate) fn arm(&self) {
        bridge::arm_screen(self.raw(), self.mode);
    }

    /// Whether a panic has not yet reset the screen since it was armed.
    pub(crate) fn armed(&self) -> bool {
        bridge::screen_armed()
    }

    /// Stops that; returns whether it was still armed, `false` once a panic already reset the screen.
    pub(crate) fn disarm(&self) -> bool {
        bridge::disarm_screen()
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _armed = bridge::disarm_screen();
        // SAFETY: `raw` is a console output handle.
        let _mode = unsafe { SetConsoleMode(self.raw(), self.mode) };
        // SAFETY: takes no pointers.
        let _code_page = unsafe { SetConsoleOutputCP(self.code_page) };
        if let Some(title) = &self.title {
            let _title = set_title(title);
        }
        OPEN.store(false, Ordering::SeqCst);
    }
}

fn open_output() -> Result<OwnedHandle, OsError> {
    let name = wide("CONOUT$");
    // SAFETY: `name` is NUL-terminated and outlives the call; no security attributes or template.
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            none(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    owned_or_invalid(raw, "CreateFileW")
}

fn mode(handle: HANDLE) -> Result<u32, OsError> {
    let mut mode = 0;
    // SAFETY: `mode` is writable; any handle value is accepted and checked.
    if unsafe { GetConsoleMode(handle, &raw mut mode) } == FALSE {
        return Err(last_error("GetConsoleMode"));
    }
    Ok(mode)
}

/// The console window's title, NUL-terminated, or `None` if it cannot be read.
fn title() -> Option<Vec<u16>> {
    let mut buffer = vec![0u16; TITLE_LIMIT];
    let size = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    clear_last_error();
    // SAFETY: `buffer` is writable for `size` UTF-16 units.
    let len = unsafe { GetConsoleTitleW(buffer.as_mut_ptr(), size) };
    // An empty title also reads as zero, but leaves no error.
    if len == 0 && last_code() != 0 {
        return None;
    }
    buffer.truncate(usize::try_from(len).ok()?.min(TITLE_LIMIT - 1));
    buffer.push(0);
    Some(buffer)
}

fn set_title(title: &[u16]) -> Result<(), OsError> {
    // SAFETY: `title` is NUL-terminated and outlives the call.
    if unsafe { SetConsoleTitleW(title.as_ptr()) } == FALSE {
        return Err(last_error("SetConsoleTitleW"));
    }
    Ok(())
}

/// Whether standard output is a console.
pub(crate) fn stdout_is_console() -> bool {
    // SAFETY: takes no pointers; the handle is borrowed from the process, never closed here.
    let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    !handle.is_null() && handle != INVALID_HANDLE_VALUE && mode(handle).is_ok()
}

/// The console output's mode, code page and title now, read through a handle of its own.
#[cfg(test)]
pub(crate) fn current() -> Result<(u32, u32, String), OsError> {
    let handle = open_output()?;
    let mode = mode(handle.as_raw_handle())?;
    // SAFETY: takes no arguments.
    let code_page = unsafe { GetConsoleOutputCP() };
    let title = title().map_or_else(String::new, |title| {
        super::from_wide(&title).to_string_lossy().into_owned()
    });
    Ok((mode, code_page, title))
}

/// Sets the console output's mode through a handle of its own.
#[cfg(test)]
pub(crate) fn set_mode(mode: u32) -> Result<(), OsError> {
    let handle = open_output()?;
    // SAFETY: `handle` is a console output handle.
    if unsafe { SetConsoleMode(handle.as_raw_handle(), mode) } == FALSE {
        return Err(last_error("SetConsoleMode"));
    }
    Ok(())
}

/// Sets the console's output code page.
#[cfg(test)]
pub(crate) fn set_code_page(code_page: u32) -> Result<(), OsError> {
    // SAFETY: takes no pointers.
    if unsafe { SetConsoleOutputCP(code_page) } == FALSE {
        return Err(last_error("SetConsoleOutputCP"));
    }
    Ok(())
}

/// Sets the console window's title without a screen.
#[cfg(test)]
pub(crate) fn set_title_for_tests(title: &str) -> Result<(), OsError> {
    set_title(&wide(title))
}
