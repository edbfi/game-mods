// SPDX-License-Identifier: AGPL-3.0-or-later
//! Waiting for the first of several Windows events: the owner thread's one blocking call.
#![forbid(unsafe_code)]

use std::os::windows::io::BorrowedHandle;
use std::time::Duration;

use crate::patch::OsError;
use crate::win32::wait;

/// The most handles one [`any`] takes (`MAXIMUM_WAIT_OBJECTS`).
pub const MAX_HANDLES: usize = wait::MAX_HANDLES;

/// Waits until one of `handles` is signalled, or `timeout` passes (`None`: no timeout).
///
/// Returns the lowest index signalled, so earlier handles take priority, or `None` on timeout. Only that
/// handle's wait is satisfied: an auto-reset event further along stays signalled for the next wait. An
/// abandoned mutex counts as signalled. The timeout is rounded up to whole milliseconds.
///
/// # Errors
///
/// The [`OsError`] if `handles` is empty or holds more than [`MAX_HANDLES`], or the wait fails.
pub fn any(
    handles: &[BorrowedHandle<'_>],
    timeout: Option<Duration>,
) -> Result<Option<usize>, OsError> {
    wait::any(handles, timeout)
}
