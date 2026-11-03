// SPDX-License-Identifier: AGPL-3.0-only
//! Playing a WAVE file held in memory.

use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

/// Starts `wav` without waiting for it, which stops the sound playing (no `SND_NOSTOP`). `wav` is `'static`
/// because the player reads it after this returns.
pub(crate) fn play(wav: &'static [u8]) {
    // SAFETY: with `SND_MEMORY` the first argument is a complete WAVE file in memory rather than a name; it
    // lives for the whole process, so the asynchronous player never reads freed memory.
    let _played = unsafe {
        PlaySoundW(
            wav.as_ptr().cast(),
            std::ptr::null_mut(),
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        )
    };
}
