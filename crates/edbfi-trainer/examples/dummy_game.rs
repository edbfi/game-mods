// SPDX-License-Identifier: AGPL-3.0-or-later
//! A stand-in game for the trainer's Windows tests. It is not a game and patches nothing itself.
//!
//! Its code holds one site of each hook shape the tests install: a predicate that returns false (a
//! return-true gate), a function that calls a battery drain (a skipped call), and a weapon fire that calls a
//! round-count setter through a table (the no-reload callback, which the game returns into). Each has a
//! unique marker immediate beside it so a test finds it by signature.
//!
//! It prints its data layout on one line, then answers one command per line on stdin:
//!
//! - `alpha`: calls the predicate; prints `alpha <0|1>`.
//! - `drain`: calls the drain; prints `battery <charge>`.
//! - `fire`: fires the player's weapon; prints `rounds <count>`.
//! - `fire-held`: fires on another thread whose setter then waits; prints `held` once it waits.
//! - `release`: lets the waiting setter finish; prints `rounds <count>`.
//!
//! Elsewhere than Windows x86-64 it does nothing.

#[cfg(all(windows, target_arch = "x86_64"))]
mod game {
    use std::io::{BufRead as _, Write as _};
    use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
    use std::sync::{Condvar, Mutex};

    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut u8;
    }

    /// The battery charge the drain spends.
    #[unsafe(no_mangle)]
    static EDBFI_DUMMY_BATTERY: AtomicU32 = AtomicU32::new(100);

    // The three sites. Each instruction is chosen for a fixed encoding the tests' profile checks.
    std::arch::global_asm!(
        ".p2align 4",
        "edbfi_dummy_alpha:",
        "mov qword ptr [rsp + 8], rbx",
        "push rdi",
        "sub rsp, 0x20",
        "xor eax, eax",
        "mov ecx, 0x5EED0001",
        "add rsp, 0x20",
        "pop rdi",
        "mov rbx, qword ptr [rsp + 8]",
        "ret",
        ".p2align 4",
        "edbfi_dummy_drain:",
        "sub rsp, 0x28",
        "mov ecx, 0x5EED0002",
        "call 2f",
        "mov eax, 0x5EED0012",
        "add rsp, 0x28",
        "ret",
        ".p2align 4",
        "2:",
        "lock dec dword ptr [rip + {battery}]",
        "ret",
        ".p2align 4",
        "edbfi_dummy_fire:",
        "push rbx",
        "sub rsp, 0x20",
        "mov rbx, rdx",
        "mov eax, dword ptr [rcx + 0x40]",
        "lea edx, [rax - 1]",
        "call qword ptr [rbx + 0x1360]",
        "mov eax, 0x5EED0003",
        "add rsp, 0x20",
        "pop rbx",
        "ret",
        ".globl edbfi_dummy_alpha",
        ".globl edbfi_dummy_drain",
        ".globl edbfi_dummy_fire",
        battery = sym EDBFI_DUMMY_BATTERY,
    );

    unsafe extern "win64" {
        fn edbfi_dummy_alpha() -> u8;
        fn edbfi_dummy_drain() -> u32;
        fn edbfi_dummy_fire(weapon: *mut u8, table: *const u8) -> u32;
    }

    /// Offsets the profile in the tests uses.
    const PLAYER: usize = 0x38;
    const OWNER: usize = 0xA0;
    const ROUNDS: usize = 0x40;
    const WEAPON_OWNER: usize = 0x118;
    const WEAPON_CONFIG: usize = 0x1B8;
    const SETTER: usize = 0x1360;

    /// The actor manager global the hooks start from.
    static MANAGER: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());
    /// Interned class ids: red, and blue under two names.
    static RED: AtomicU32 = AtomicU32::new(0x7E0_0001);
    static BLUE: AtomicU32 = AtomicU32::new(0x7E0_0002);
    static BLUE_ALIAS: AtomicU32 = AtomicU32::new(0x7E0_0003);

    /// Whether the setter waits, and the hand-off with the waiting thread.
    static HOLD: Mutex<Hold> = Mutex::new(Hold::Off);
    static CHANGED: Condvar = Condvar::new();

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Hold {
        Off,
        Armed,
        Waiting,
        Released,
    }

    /// The round-count setter the fire site calls through its table.
    extern "win64" fn set_rounds(weapon: *mut u8, rounds: u32) {
        let mut hold = HOLD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *hold == Hold::Armed {
            *hold = Hold::Waiting;
            CHANGED.notify_all();
            while *hold != Hold::Released {
                hold = CHANGED
                    .wait(hold)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            *hold = Hold::Off;
        }
        // SAFETY: `weapon` is the leaked weapon block, at least `ROUNDS + 4` bytes and 8-aligned.
        unsafe { weapon.wrapping_add(ROUNDS).cast::<u32>().write(rounds) };
    }

    /// A leaked, zeroed, 16-aligned block of `len` bytes.
    fn block(len: usize) -> *mut u8 {
        Box::leak(vec![0u128; len.div_ceil(16)].into_boxed_slice())
            .as_mut_ptr()
            .cast()
    }

    /// Writes `value` at `offset` into `block`.
    ///
    /// # Safety
    ///
    /// `block` must be valid for 8 writable, 8-aligned bytes at `offset`.
    unsafe fn put(block: *mut u8, offset: usize, value: usize) {
        // SAFETY: the caller's contract.
        unsafe { block.wrapping_add(offset).cast::<usize>().write(value) };
    }

    struct World {
        weapon: *mut u8,
        table: *mut u8,
    }

    // SAFETY: the blocks are leaked and only touched through the game functions and the setter.
    unsafe impl Send for World {}

    /// The player, its weapon of the red class, and the setter table.
    fn world() -> World {
        let manager = block(PLAYER + 8);
        let player = block(OWNER + 8);
        let handle = block(8);
        let config = block(8);
        let weapon = block(WEAPON_CONFIG + 8);
        let table = block(SETTER + 8);
        // SAFETY: each block was just allocated large enough and 16-aligned for the offset written.
        unsafe {
            put(manager, PLAYER, player as usize);
        }
        // SAFETY: as above.
        unsafe { put(handle, 0, player as usize + OWNER) };
        // SAFETY: as above.
        unsafe { put(weapon, WEAPON_OWNER, handle as usize) };
        // SAFETY: as above.
        unsafe { put(weapon, WEAPON_CONFIG, config as usize) };
        // SAFETY: as above.
        unsafe { put(table, SETTER, set_rounds as *const () as usize) };
        // SAFETY: as above; the config's first DWORD is the class id.
        unsafe { config.cast::<u32>().write(RED.load(Ordering::Relaxed)) };
        // SAFETY: as above.
        unsafe { weapon.wrapping_add(ROUNDS).cast::<u32>().write(30) };
        MANAGER.store(manager, Ordering::SeqCst);
        World { weapon, table }
    }

    fn fire(world: &World) -> u32 {
        // SAFETY: the weapon and table are the world's, laid out as the fire site reads them.
        unsafe { edbfi_dummy_fire(world.weapon, world.table) };
        // SAFETY: as above.
        unsafe { world.weapon.wrapping_add(ROUNDS).cast::<u32>().read() }
    }

    pub(super) fn run() {
        // SAFETY: a null name asks for this executable's own module handle.
        let image = unsafe { GetModuleHandleW(std::ptr::null()) } as usize;
        let rva = |address: usize| address - image;
        let world = world();
        let mut out = std::io::stdout().lock();
        let _ = writeln!(
            out,
            "layout manager={:#x} red={:#x} blue={:#x} blue_alias={:#x}",
            rva(std::ptr::from_ref(&MANAGER) as usize),
            rva(std::ptr::from_ref(&RED) as usize),
            rva(std::ptr::from_ref(&BLUE) as usize),
            rva(std::ptr::from_ref(&BLUE_ALIAS) as usize),
        );
        let _ = out.flush();
        let mut held = None;
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let reply = match line.trim() {
                // SAFETY: the predicate takes nothing and touches nothing.
                "alpha" => format!("alpha {}", unsafe { edbfi_dummy_alpha() }),
                "drain" => {
                    // SAFETY: the drain only decrements the battery.
                    unsafe { edbfi_dummy_drain() };
                    format!("battery {}", EDBFI_DUMMY_BATTERY.load(Ordering::SeqCst))
                }
                "fire" => format!("rounds {}", fire(&world)),
                "fire-held" => {
                    *HOLD
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Hold::Armed;
                    let shared = World {
                        weapon: world.weapon,
                        table: world.table,
                    };
                    held = Some(std::thread::spawn(move || fire(&shared)));
                    let mut hold = HOLD
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    while *hold != Hold::Waiting {
                        hold = CHANGED
                            .wait(hold)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                    "held".to_owned()
                }
                "release" => {
                    *HOLD
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Hold::Released;
                    CHANGED.notify_all();
                    match held.take().map(std::thread::JoinHandle::join) {
                        Some(Ok(rounds)) => format!("rounds {rounds}"),
                        _ => "error".to_owned(),
                    }
                }
                _ => "unknown".to_owned(),
            };
            let _ = writeln!(out, "{reply}");
            let _ = out.flush();
        }
    }
}

fn main() {
    #[cfg(all(windows, target_arch = "x86_64"))]
    game::run();
}
