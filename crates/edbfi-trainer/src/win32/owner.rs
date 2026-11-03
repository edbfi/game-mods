// SPDX-License-Identifier: AGPL-3.0-only
//! The named objects that make a trainer the one owner of a game process, and the stop request other
//! trainer processes send it.
//!
//! The names hold the mod's folders and the process id, never a version, so any two builds of a trainer
//! exclude each other. Either object already existing means another trainer owns the game. Named objects
//! are per logon session (`Local\`).

use std::os::windows::io::{AsHandle as _, AsRawHandle as _, BorrowedHandle, OwnedHandle};

use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, FALSE, TRUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, OpenEventW, SetEvent, WaitForSingleObject,
};

use crate::profile::ModSlug;
use crate::session::{ClaimError, Owner, StopOutcome};

use super::process::none;
use super::{clear_last_error, last_code, last_error, owned, wide};

/// The owner mutex's name for process `pid`.
fn owner_name(slug: ModSlug, pid: u32) -> String {
    format!("Local\\edbfi.{}.{}.{pid}", slug.game, slug.r#mod)
}

/// The stop event's name for process `pid`.
fn stop_name(slug: ModSlug, pid: u32) -> String {
    format!("Local\\edbfi.{}.{}.{pid}.stop", slug.game, slug.r#mod)
}

/// This trainer's ownership of one game process.
pub(crate) struct WinOwner {
    _mutex: OwnedHandle,
    stop: OwnedHandle,
}

impl Owner for WinOwner {
    fn stop_requested(&self) -> bool {
        // SAFETY: the stop event is a live handle this owner holds.
        unsafe { WaitForSingleObject(self.stop.as_raw_handle(), 0) == WAIT_OBJECT_0 }
    }

    fn stop_event(&self) -> Option<BorrowedHandle<'_>> {
        Some(self.stop.as_handle())
    }
}

/// Creates a named object with `create`, refusing one that already exists.
fn create_new(
    create: impl FnOnce() -> windows_sys::Win32::Foundation::HANDLE,
    call: &'static str,
) -> Result<OwnedHandle, ClaimError> {
    clear_last_error();
    let raw = create();
    let existed = last_code() == ERROR_ALREADY_EXISTS;
    let handle = owned(raw, call).map_err(ClaimError::Os)?;
    if existed {
        return Err(ClaimError::Owned);
    }
    Ok(handle)
}

/// Takes ownership of process `pid` for the mod `slug`.
pub(crate) fn claim(slug: ModSlug, pid: u32) -> Result<WinOwner, ClaimError> {
    let mutex = wide(&owner_name(slug, pid));
    // SAFETY: `mutex` is a NUL-terminated UTF-16 name; the attributes are null.
    let mutex = create_new(
        || unsafe { CreateMutexW(none(), FALSE, mutex.as_ptr()) },
        "CreateMutexW",
    )?;
    let stop = wide(&stop_name(slug, pid));
    // SAFETY: as above, for a manual-reset, unsignalled event.
    let stop = create_new(
        || unsafe { CreateEventW(none(), TRUE, FALSE, stop.as_ptr()) },
        "CreateEventW",
    )?;
    Ok(WinOwner {
        _mutex: mutex,
        stop,
    })
}

/// Sets the stop event of the trainer owning process `pid`, without opening the game or creating anything.
pub(crate) fn request_stop(slug: ModSlug, pid: u32) -> StopOutcome {
    let name = wide(&stop_name(slug, pid));
    // SAFETY: `name` is a NUL-terminated UTF-16 name.
    let raw = unsafe { OpenEventW(EVENT_MODIFY_STATE, FALSE, name.as_ptr()) };
    let not_found = raw.is_null() && last_code() == ERROR_FILE_NOT_FOUND;
    match owned(raw, "OpenEventW") {
        Err(_) if not_found => StopOutcome::NoTrainer,
        Err(error) => StopOutcome::Failed(error),
        Ok(event) => {
            // SAFETY: `event` is a live event handle with modify rights.
            if unsafe { SetEvent(event.as_raw_handle()) } == FALSE {
                StopOutcome::Failed(last_error("SetEvent"))
            } else {
                StopOutcome::Requested
            }
        }
    }
}
