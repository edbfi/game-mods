// SPDX-License-Identifier: AGPL-3.0-only
//! The trainer's console input: its modes, and its records as the arbiter reads them.
//!
//! The reader opens `CONIN$`, so it reads the console even when standard input is redirected. It turns off
//! QuickEdit, mouse, line, echo and virtual-terminal input, so keys arrive as virtual-key records the moment
//! they are typed and a click cannot freeze the trainer by selecting text; it keeps processed input (Ctrl+C
//! stays a control notice) and window input (size changes). Dropping it restores the exact modes it found.

use std::os::windows::io::{AsHandle as _, AsRawHandle as _, BorrowedHandle, OwnedHandle};

use windows_sys::Win32::Foundation::{FALSE, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT, ENABLE_MOUSE_INPUT,
    ENABLE_PROCESSED_INPUT, ENABLE_QUICK_EDIT_MODE, ENABLE_VIRTUAL_TERMINAL_INPUT,
    ENABLE_WINDOW_INPUT, FOCUS_EVENT, FlushConsoleInputBuffer, GetConsoleMode,
    GetNumberOfConsoleInputEvents, INPUT_RECORD, KEY_EVENT, LEFT_ALT_PRESSED, LEFT_CTRL_PRESSED,
    RIGHT_ALT_PRESSED, RIGHT_CTRL_PRESSED, ReadConsoleInputW, SHIFT_PRESSED, SetConsoleMode,
    WINDOW_BUFFER_SIZE_EVENT,
};

use crate::chord::Modifiers;
use crate::input::{Batch, KeyRecord, Record};
use crate::patch::OsError;

use super::process::none;
use super::{last_error, owned_or_invalid, wide};

/// The most records one read takes; more waiting makes the batch partial.
pub(crate) const BATCH: usize = 64;

/// The most copies one record's repeat count expands to.
const REPEAT_LIMIT: u16 = 16;

/// The modes the reader turns off.
const OFF: u32 = ENABLE_QUICK_EDIT_MODE
    | ENABLE_MOUSE_INPUT
    | ENABLE_LINE_INPUT
    | ENABLE_ECHO_INPUT
    | ENABLE_VIRTUAL_TERMINAL_INPUT;
/// The modes the reader keeps on. Extended flags make the QuickEdit bit take effect.
const ON: u32 = ENABLE_PROCESSED_INPUT | ENABLE_WINDOW_INPUT | ENABLE_EXTENDED_FLAGS;

/// The console's input, with the reader's modes in effect.
pub(crate) struct Reader {
    handle: OwnedHandle,
    original: u32,
}

impl Reader {
    /// Opens the console's input and sets the reader's modes.
    pub(crate) fn open() -> Result<Self, OsError> {
        let name = wide("CONIN$");
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
        let handle = owned_or_invalid(raw, "CreateFileW")?;
        let mut original = 0;
        // SAFETY: `handle` is a console input handle and `original` is writable.
        if unsafe { GetConsoleMode(handle.as_raw_handle(), &raw mut original) } == FALSE {
            return Err(last_error("GetConsoleMode"));
        }
        let reader = Self { handle, original };
        reader.set((original | ON) & !OFF)?;
        Ok(reader)
    }

    fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle()
    }

    fn set(&self, mode: u32) -> Result<(), OsError> {
        // SAFETY: `raw` is a console input handle.
        if unsafe { SetConsoleMode(self.raw(), mode) } == FALSE {
            return Err(last_error("SetConsoleMode"));
        }
        Ok(())
    }

    /// The modes in effect.
    #[cfg(test)]
    pub(crate) fn mode(&self) -> u32 {
        let mut mode = 0;
        // SAFETY: as in `open`.
        let _read = unsafe { GetConsoleMode(self.raw(), &raw mut mode) };
        mode
    }

    /// The modes the reader found, which dropping it restores.
    #[cfg(test)]
    pub(crate) const fn original(&self) -> u32 {
        self.original
    }

    /// The input handle, signalled while records wait.
    pub(crate) fn handle(&self) -> BorrowedHandle<'_> {
        self.handle.as_handle()
    }

    fn waiting(&self) -> Result<u32, OsError> {
        let mut count = 0;
        // SAFETY: `count` is writable.
        if unsafe { GetNumberOfConsoleInputEvents(self.raw(), &raw mut count) } == FALSE {
            return Err(last_error("GetNumberOfConsoleInputEvents"));
        }
        Ok(count)
    }

    /// Reads up to [`BATCH`] waiting records without blocking. A batch is complete if nothing waits after it.
    pub(crate) fn read(&mut self) -> Result<Batch, OsError> {
        let mut batch = Batch {
            complete: true,
            ..Batch::default()
        };
        let waiting = self.waiting()?;
        if waiting == 0 {
            return Ok(batch);
        }
        let mut buffer = [INPUT_RECORD::default(); BATCH];
        let wanted = waiting.min(u32::try_from(BATCH).unwrap_or(u32::MAX));
        let mut read = 0;
        // SAFETY: `buffer` has room for `wanted` records and `read` is writable. Records are waiting, so the
        // call returns at once.
        if unsafe { ReadConsoleInputW(self.raw(), buffer.as_mut_ptr(), wanted, &raw mut read) }
            == FALSE
        {
            return Err(last_error("ReadConsoleInputW"));
        }
        for record in buffer.iter().take(usize::try_from(read).unwrap_or(BATCH)) {
            convert(record, &mut batch);
        }
        batch.complete = self.waiting()? == 0;
        Ok(batch)
    }

    /// Discards every waiting record.
    pub(crate) fn flush(&mut self) -> Result<(), OsError> {
        // SAFETY: `raw` is a console input handle.
        if unsafe { FlushConsoleInputBuffer(self.raw()) } == FALSE {
            return Err(last_error("FlushConsoleInputBuffer"));
        }
        Ok(())
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let _restored = self.set(self.original);
    }
}

fn convert(record: &INPUT_RECORD, batch: &mut Batch) {
    match u32::from(record.EventType) {
        KEY_EVENT => {
            // SAFETY: the event type says which union member Windows wrote.
            let key = unsafe { record.Event.KeyEvent };
            let state = key.dwControlKeyState;
            let mut modifiers = Modifiers::NONE;
            for (bits, modifier) in [
                (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED, Modifiers::CTRL),
                (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED, Modifiers::ALT),
                (SHIFT_PRESSED, Modifiers::SHIFT),
            ] {
                if state & bits != 0 {
                    modifiers = modifiers.union(modifier);
                }
            }
            // SAFETY: the `W` call writes the UTF-16 member.
            let unit = unsafe { key.uChar.UnicodeChar };
            let record = Record::Key(KeyRecord {
                vk: key.wVirtualKeyCode,
                down: key.bKeyDown != FALSE,
                modifiers,
                text: (unit != 0)
                    .then(|| char::from_u32(u32::from(unit)))
                    .flatten(),
            });
            let copies = if key.bKeyDown == FALSE {
                1
            } else {
                key.wRepeatCount.clamp(1, REPEAT_LIMIT)
            };
            batch
                .records
                .extend(std::iter::repeat_n(record, usize::from(copies)));
        }
        FOCUS_EVENT => batch.records.push(Record::Focus),
        WINDOW_BUFFER_SIZE_EVENT => batch.resized = true,
        _ => {}
    }
}
