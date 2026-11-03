// SPDX-License-Identifier: AGPL-3.0-only
//! Waiting on several handles at once.

use std::os::windows::io::{AsRawHandle as _, BorrowedHandle};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    ERROR_INVALID_PARAMETER, FALSE, HANDLE, WAIT_ABANDONED_0, WAIT_FAILED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Threading::{INFINITE, WaitForMultipleObjects};

use crate::patch::OsError;

use super::{last_error, millis};

/// The most handles one wait takes (`MAXIMUM_WAIT_OBJECTS`).
pub(crate) const MAX_HANDLES: usize = 64;

const CALL: &str = "WaitForMultipleObjects";

/// Waits until one of `handles` is signalled or `timeout` passes, and returns the lowest signalled index.
pub(crate) fn any(
    handles: &[BorrowedHandle<'_>],
    timeout: Option<Duration>,
) -> Result<Option<usize>, OsError> {
    if handles.is_empty() || handles.len() > MAX_HANDLES {
        return Err(OsError::new(CALL, ERROR_INVALID_PARAMETER));
    }
    let mut raw: [HANDLE; MAX_HANDLES] = [std::ptr::null_mut(); MAX_HANDLES];
    for (slot, handle) in raw.iter_mut().zip(handles) {
        *slot = handle.as_raw_handle();
    }
    let count =
        u32::try_from(handles.len()).map_err(|_| OsError::new(CALL, ERROR_INVALID_PARAMETER))?;
    let wait = timeout.map_or(INFINITE, millis);
    // SAFETY: the first `count` entries of `raw` are handles borrowed for the length of the call.
    let result = unsafe { WaitForMultipleObjects(count, raw.as_ptr(), FALSE, wait) };
    let index = |base: u32| {
        result
            .checked_sub(base)
            .filter(|&offset| offset < count)
            .and_then(|offset| usize::try_from(offset).ok())
    };
    if result == WAIT_TIMEOUT {
        return Ok(None);
    }
    if result == WAIT_FAILED {
        return Err(last_error(CALL));
    }
    // A mutex whose owner exited without releasing it is still signalled; the caller now owns it.
    index(WAIT_OBJECT_0)
        .or_else(|| index(WAIT_ABANDONED_0))
        .map(Some)
        .ok_or(OsError::new(CALL, result))
}
