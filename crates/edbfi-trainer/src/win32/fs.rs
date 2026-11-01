// SPDX-License-Identifier: AGPL-3.0-only
//! File replacement that hands back the replaced file.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Storage::FileSystem::{REPLACEFILE_IGNORE_MERGE_ERRORS, ReplaceFileW};

/// `path` as a NUL-terminated UTF-16 string.
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let wide: Vec<u16> = OsStr::encode_wide(path.as_os_str()).collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a path contains a NUL character",
        ));
    }
    Ok(wide.into_iter().chain([0]).collect())
}

/// Replaces `target` with `replacement` and moves the replaced file to `backup`, which must not exist.
///
/// `target` must exist; if it does not, the error's kind is [`io::ErrorKind::NotFound`]. On failure `target`
/// normally keeps its contents, but Windows may have already moved it to `backup` without moving `replacement`
/// into place (`ERROR_UNABLE_TO_MOVE_REPLACEMENT_2`); the caller must then put `backup` back.
pub(crate) fn replace_file(target: &Path, replacement: &Path, backup: &Path) -> io::Result<()> {
    let (target, replacement, backup) = (wide(target)?, wide(replacement)?, wide(backup)?);
    // SAFETY: the three paths are NUL-terminated UTF-16 buffers that outlive the call; the exclude and reserved
    // pointers must be null.
    let replaced = unsafe {
        ReplaceFileW(
            target.as_ptr(),
            replacement.as_ptr(),
            backup.as_ptr(),
            REPLACEFILE_IGNORE_MERGE_ERRORS,
            ptr::null(),
            ptr::null(),
        )
    };
    if replaced == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
