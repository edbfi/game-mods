// SPDX-License-Identifier: AGPL-3.0-only
//! Safe wrappers over the Win32 calls the trainer needs. This is the only module with `unsafe` code; every
//! `unsafe` block states why it is sound.

pub(crate) mod bridge;
pub(crate) mod console;
pub(crate) mod controls;
pub(crate) mod fs;
pub(crate) mod input;
pub(crate) mod owner;
pub(crate) mod process;
pub(crate) mod window;

use std::os::windows::io::{HandleOrInvalid, HandleOrNull, OwnedHandle};

use windows_sys::Win32::Foundation::{GetLastError, HANDLE, SetLastError};

use crate::patch::OsError;

/// The calling thread's last error, as the failure of `call`.
fn last_error(call: &'static str) -> OsError {
    // SAFETY: `GetLastError` only reads the calling thread's last-error value.
    OsError::new(call, unsafe { GetLastError() })
}

/// The calling thread's last error code.
fn last_code() -> u32 {
    // SAFETY: as in `last_error`.
    unsafe { GetLastError() }
}

/// Clears the calling thread's last error, for calls that report "already exists" only through it.
fn clear_last_error() {
    // SAFETY: `SetLastError` only writes the calling thread's last-error value.
    unsafe { SetLastError(0) }
}

/// Takes ownership of a handle from a call that returns null on failure.
fn owned(raw: HANDLE, call: &'static str) -> Result<OwnedHandle, OsError> {
    let error = last_error(call);
    // SAFETY: `raw` was just returned by a Win32 call that returns either null or a new handle the caller
    // owns, and nothing else holds it.
    let handle = unsafe { HandleOrNull::from_raw_handle(raw) };
    OwnedHandle::try_from(handle).map_err(|_| error)
}

/// Takes ownership of a handle from a call that returns `INVALID_HANDLE_VALUE` on failure.
fn owned_or_invalid(raw: HANDLE, call: &'static str) -> Result<OwnedHandle, OsError> {
    let error = last_error(call);
    // SAFETY: `raw` was just returned by a Win32 call that returns either `INVALID_HANDLE_VALUE` or a new
    // handle the caller owns, and nothing else holds it.
    let handle = unsafe { HandleOrInvalid::from_raw_handle(raw) };
    OwnedHandle::try_from(handle).map_err(|_| error)
}

/// `text` as a NUL-terminated UTF-16 string.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// The text of a NUL-terminated UTF-16 buffer.
fn from_wide(buffer: &[u16]) -> std::ffi::OsString {
    use std::os::windows::ffi::OsStringExt as _;
    let len = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    std::ffi::OsString::from_wide(&buffer[..len])
}

/// Milliseconds for a Win32 wait: rounded up, and never `INFINITE`.
fn millis(timeout: std::time::Duration) -> u32 {
    let ms = timeout.as_nanos().div_ceil(1_000_000);
    u32::try_from(ms).unwrap_or(u32::MAX - 1).min(u32::MAX - 1)
}

#[cfg(test)]
mod tests;
