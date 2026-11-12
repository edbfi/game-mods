// SPDX-License-Identifier: AGPL-3.0-or-later
//! Keyboard and foreground notices for the owner thread, heard on the hidden window's thread.
//!
//! The hidden top-level window registers for Raw Input from every keyboard, even while another window has
//! the focus, and for keyboard arrival and removal; a WinEvent hook on the same thread hears every foreground
//! change. None of these notices carries authority: each only sets a flag and signals one auto-reset event,
//! and the owner then asks what is held and what is in front. Flags are set before the event is signalled,
//! and the owner clears them after waking, so no notice is lost and repeated notices merge; nothing can
//! overflow. A barrier message, posted by the owner, is answered after every keyboard notice already queued.

use std::os::windows::io::{AsHandle as _, AsRawHandle as _, BorrowedHandle, OwnedHandle};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use windows_sys::Win32::Foundation::{FALSE, HWND, LPARAM, WPARAM};
use windows_sys::Win32::System::Console::GetConsoleWindow;
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows_sys::Win32::UI::Input::{
    RAWINPUTDEVICE, RIDEV_DEVNOTIFY, RIDEV_INPUTSINK, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EVENT_SYSTEM_FOREGROUND, GetForegroundWindow, GetWindowThreadProcessId,
    IsWindowVisible, MSG, PM_REMOVE, PeekMessageW, PostMessageW, WINEVENT_OUTOFCONTEXT, WM_APP,
    WM_INPUT, WM_INPUT_DEVICE_CHANGE,
};

use crate::input::Wakes;
use crate::input::{ConsoleFocus, Epoch};
use crate::patch::OsError;

use super::process::none;
use super::{last_error, owned};

/// The owner's barrier; `wparam` carries the epoch.
pub(super) const WM_BARRIER: u32 = WM_APP + 1;

/// How many queued keyboard notices a barrier handles before it answers, so a flood cannot hold it forever.
const DRAIN_LIMIT: usize = 4096;

const USAGE_PAGE_GENERIC: u16 = 0x01;
const USAGE_KEYBOARD: u16 = 0x06;

/// The flags and the event that wakes the owner.
pub(crate) struct Notices {
    event: OwnedHandle,
    window: usize,
    keyboard: AtomicBool,
    devices: AtomicBool,
    foreground: AtomicBool,
    /// The last acknowledged epoch plus one; zero for none.
    barrier: AtomicU64,
}

static NOTICES: OnceLock<Result<Notices, OsError>> = OnceLock::new();

impl Notices {
    fn signal(&self, flag: &AtomicBool) {
        flag.store(true, Ordering::SeqCst);
        self.ring();
    }

    fn ring(&self) {
        // SAFETY: the event lives for the whole process.
        let _set = unsafe { SetEvent(self.event.as_raw_handle()) };
    }

    /// Clears and returns what happened.
    pub(crate) fn take(&self) -> Wakes {
        let barrier = self.barrier.swap(0, Ordering::SeqCst);
        Wakes {
            keyboard: self.keyboard.swap(false, Ordering::SeqCst),
            devices: self.devices.swap(false, Ordering::SeqCst),
            foreground: self.foreground.swap(false, Ordering::SeqCst),
            barrier: barrier.checked_sub(1).map(Epoch::from_raw),
        }
    }

    /// The event signalled after each notice.
    pub(crate) fn event(&'static self) -> BorrowedHandle<'static> {
        self.event.as_handle()
    }

    /// Asks the window thread to acknowledge `epoch` once it has handled every keyboard notice before it.
    pub(crate) fn barrier(&self, epoch: Epoch) -> Result<(), OsError> {
        let wparam = usize::try_from(epoch.get()).map_err(|_| OsError::new("PostMessageW", 87))?;
        // SAFETY: the window lives for the whole process; the message carries only integers.
        let posted = unsafe { PostMessageW(self.window as HWND, WM_BARRIER, wparam, 0) };
        if posted == FALSE {
            return Err(last_error("PostMessageW"));
        }
        Ok(())
    }
}

/// The notices, once the window thread set them up.
pub(crate) fn notices() -> Option<Result<&'static Notices, OsError>> {
    NOTICES.get().map(Result::as_ref).map(|r| r.map_err(|e| *e))
}

/// Sets up the notices for `window`, on its thread. A failure is kept for [`notices`] to report; the window
/// still serves session end.
pub(super) fn start(window: HWND) {
    let _set = NOTICES.set(register(window));
}

fn register(window: HWND) -> Result<Notices, OsError> {
    // SAFETY: an auto-reset, unsignalled, unnamed event takes no pointers but the null ones.
    let raw = unsafe { CreateEventW(none(), FALSE, FALSE, none()) };
    let event = owned(raw, "CreateEventW")?;
    let keyboards = RAWINPUTDEVICE {
        usUsagePage: USAGE_PAGE_GENERIC,
        usUsage: USAGE_KEYBOARD,
        dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
        hwndTarget: window,
    };
    let size = u32::try_from(size_of::<RAWINPUTDEVICE>()).unwrap_or(u32::MAX);
    // SAFETY: one fully initialised registration for a window of this thread.
    if unsafe { RegisterRawInputDevices(&raw const keyboards, 1, size) } == FALSE {
        return Err(last_error("RegisterRawInputDevices"));
    }
    // SAFETY: an out-of-context hook needs no module; its callback lives for the whole process and runs on
    // this thread, which pumps messages until the process ends.
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            std::ptr::null_mut(),
            Some(foreground_changed),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    if hook.is_null() {
        return Err(last_error("SetWinEventHook"));
    }
    Ok(Notices {
        event,
        window: window as usize,
        keyboard: AtomicBool::new(false),
        devices: AtomicBool::new(false),
        foreground: AtomicBool::new(false),
        barrier: AtomicU64::new(0),
    })
}

/// Handles a window message that concerns input. Returns `true` if the message needs no default handling.
pub(super) fn heard(message: u32, wparam: WPARAM, _lparam: LPARAM) -> bool {
    let Some(Ok(notices)) = notices() else {
        return false;
    };
    match message {
        WM_INPUT => notices.signal(&notices.keyboard),
        WM_INPUT_DEVICE_CHANGE => notices.signal(&notices.devices),
        WM_BARRIER => {
            drain();
            notices.barrier.store(
                u64::try_from(wparam).unwrap_or(u64::MAX).saturating_add(1),
                Ordering::SeqCst,
            );
            notices.ring();
            return true;
        }
        _ => {}
    }
    false
}

/// Handles the keyboard notices already queued for this thread, which Windows would deliver after posted
/// messages such as the barrier.
fn drain() {
    // SAFETY: plain data, valid when zeroed.
    let mut message: MSG = unsafe { std::mem::zeroed() };
    for _ in 0..DRAIN_LIMIT {
        // SAFETY: `message` is writable; a null window takes this thread's messages, filtered to Raw Input.
        let found = unsafe {
            PeekMessageW(
                &raw mut message,
                std::ptr::null_mut(),
                WM_INPUT,
                WM_INPUT,
                PM_REMOVE,
            )
        };
        if found == FALSE {
            return;
        }
        // SAFETY: `message` came from `PeekMessageW`; dispatching it runs the window procedure.
        let _ = unsafe { DispatchMessageW(&raw const message) };
    }
}

/// The foreground hook's callback.
unsafe extern "system" fn foreground_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _window: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    if let Some(Ok(notices)) = notices() {
        notices.signal(&notices.foreground);
    }
}

/// Runs the foreground hook's callback as Windows would, for tests.
#[cfg(test)]
pub(crate) fn foreground_notice() {
    // SAFETY: the callback ignores its arguments.
    unsafe { foreground_changed(std::ptr::null_mut(), 0, std::ptr::null_mut(), 0, 0, 0, 0) };
}

/// Whether the key with virtual-key code `vk` is physically down.
pub(crate) fn key_down(vk: u8) -> bool {
    // SAFETY: `GetAsyncKeyState` only reads the system's key state.
    let state = unsafe { GetAsyncKeyState(i32::from(vk)) };
    state < 0
}

/// The process owning the foreground window.
pub(crate) fn foreground() -> Option<u32> {
    // SAFETY: takes no arguments.
    let window = unsafe { GetForegroundWindow() };
    if window.is_null() {
        return None;
    }
    let mut pid = 0;
    // SAFETY: `pid` is writable; a window that has gone away yields zero.
    let _thread = unsafe { GetWindowThreadProcessId(window, &raw mut pid) };
    (pid != 0).then_some(pid)
}

/// Where this process's console window is.
pub(crate) fn console_focus() -> ConsoleFocus {
    // SAFETY: takes no arguments.
    let console = unsafe { GetConsoleWindow() };
    // SAFETY: `IsWindowVisible` accepts any window handle, including a stale or null one.
    if console.is_null() || unsafe { IsWindowVisible(console) } == FALSE {
        // No console, or the hidden window a pseudo console reports.
        return ConsoleFocus::Hidden;
    }
    // SAFETY: takes no arguments.
    if unsafe { GetForegroundWindow() } == console {
        ConsoleFocus::Front
    } else {
        ConsoleFocus::Behind
    }
}
