// SPDX-License-Identifier: AGPL-3.0-only
//! The hidden top-level window that hears about logoff and shutdown.
//!
//! A console process that loads `user32.dll` no longer gets logoff or shutdown console notices; Windows sends
//! `WM_QUERYENDSESSION` and `WM_ENDSESSION` to its top-level windows instead, never to message-only windows.
//! This window never shows. Its thread runs only the message pump and never touches the game: on
//! `WM_QUERYENDSESSION` it zeroes the controls, asks the owner to restore, and waits for that within the
//! budget before allowing the session to end; `WM_ENDSESSION` waits again, which returns at once if
//! restoration already finished. A cancelled session end changes nothing. If the pump ever stops or panics,
//! the owner is asked to restore and exit.

use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, RegisterClassW,
    TranslateMessage, WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSW, WS_OVERLAPPED,
};

use crate::patch::OsError;

use super::bridge::{self, Bridge};
use super::process::none;
use super::{last_error, wide};

/// The window, once created, for tests to send messages to.
static WINDOW: OnceLock<usize> = OnceLock::new();

/// The window procedure.
unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ending = match message {
        WM_QUERYENDSESSION => Some(1),
        WM_ENDSESSION if wparam != 0 => Some(0),
        _ => None,
    };
    if let Some(answer) = ending {
        let handled = std::panic::catch_unwind(|| {
            if let Some(bridge) = bridge::get() {
                bridge.ending();
            }
        });
        if handled.is_err()
            && let Some(bridge) = bridge::get()
        {
            bridge.request();
        }
        return answer;
    }
    // SAFETY: every other message gets the default handling, with the arguments Windows passed.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// Creates the window on a thread of its own and pumps its messages until the process ends.
pub(super) fn start(bridge: &'static Bridge) -> Result<(), OsError> {
    let (ready, created) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("edbfi-session-end".into())
        .spawn(move || {
            let pumped = std::panic::catch_unwind(|| match create() {
                Ok(window) => {
                    let _sent = ready.send(Ok(()));
                    let _set = WINDOW.set(window as usize);
                    pump();
                }
                Err(error) => {
                    let _sent = ready.send(Err(error));
                }
            });
            // The pump never ends while the process runs; if it does, the owner must not keep hooks in place
            // unheard.
            let _ = pumped;
            bridge.request();
        })
        .map_err(|_| OsError::new("CreateThread", 8))?;
    created
        .recv()
        .unwrap_or(Err(OsError::new("CreateWindowExW", 0)))
}

fn create() -> Result<HWND, OsError> {
    let class = wide("edbfi.session-end");
    // SAFETY: a null name asks for this executable's module handle.
    let instance = unsafe { GetModuleHandleW(none()) };
    let definition = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(procedure),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: std::ptr::null_mut(),
        hCursor: std::ptr::null_mut(),
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: none(),
        lpszClassName: class.as_ptr(),
    };
    // SAFETY: `definition` is fully initialised and its class name outlives the call.
    if unsafe { RegisterClassW(&raw const definition) } == 0 {
        return Err(last_error("RegisterClassW"));
    }
    // SAFETY: the class is registered; a hidden, parentless (top-level) window with no menu or data.
    let window = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            none(),
        )
    };
    if window.is_null() {
        return Err(last_error("CreateWindowExW"));
    }
    Ok(window)
}

fn pump() {
    // SAFETY: plain data, valid when zeroed.
    let mut message: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `message` is writable; a null window takes every message of this thread.
        let got = unsafe { GetMessageW(&raw mut message, std::ptr::null_mut(), 0, 0) };
        if got <= 0 {
            return;
        }
        // SAFETY: `message` came from `GetMessageW`.
        let _translated = unsafe { TranslateMessage(&raw const message) };
        // SAFETY: as above.
        let _ = unsafe { DispatchMessageW(&raw const message) };
    }
}

/// The hidden window, once created.
#[cfg(test)]
pub(crate) fn window() -> Option<HWND> {
    WINDOW.get().map(|&window| window as HWND)
}
