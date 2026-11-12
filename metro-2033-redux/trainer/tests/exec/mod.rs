// SPDX-License-Identifier: AGPL-3.0-or-later
//! Linux x86-64 execution harness for generated hook caves: the one place test code runs machine code.
//!
//! An [`Arena`] is one anonymous mapping that stands in for the game: the image at its base, then the caves,
//! the control word, game objects and stacks. Data pages stay read-write; [`Arena::code`] writes a code range
//! while its pages are read-write and then makes them read-execute, so nothing is ever writable and executable
//! at once.
//!
//! A [`Machine`] is a generated System V function that switches to a test stack, loads every general register,
//! RFLAGS, all sixteen XMM registers and MXCSR from a [`State`], enters a target as if called, and on return
//! captures the same state before restoring the host's callee-saved registers, stack and MXCSR. The target
//! sees `[rsp]` holding a return address into the machine, as a called function would.

use std::ptr::NonNull;

use iced_x86::Register;
use iced_x86::code_asm::{
    CodeAssembler, dword_ptr, get_gpr64, qword_ptr, r12, r13, r14, r15, rax, rbp, rbx, rsp, xmm0,
    xmm1, xmm2, xmm3, xmm4, xmm5, xmm6, xmm7, xmm8, xmm9, xmm10, xmm11, xmm12, xmm13, xmm14, xmm15,
    xmmword_ptr,
};

/// Bytes per page.
pub(crate) const PAGE: usize = 4096;
/// The bytes a fresh stack holds, so a test can see which slots were written.
pub(crate) const CANARY: u8 = 0x5A;

/// RFLAGS bits a seed may set: CF, PF, AF, ZF, SF and OF.
pub(crate) const ARITHMETIC_FLAGS: u64 = 0x8D5;
/// RFLAGS bits a capture keeps: the arithmetic flags, DF and the always-set bit 1. The rest (IF, for one) are
/// the kernel's.
pub(crate) const CAPTURED_FLAGS: u64 = ARITHMETIC_FLAGS | 0x400 | 2;

/// The register order of [`State::gpr`]: the hardware numbering, RAX to R15.
pub(crate) const GPRS: [Register; 16] = [
    Register::RAX,
    Register::RCX,
    Register::RDX,
    Register::RBX,
    Register::RSP,
    Register::RBP,
    Register::RSI,
    Register::RDI,
    Register::R8,
    Register::R9,
    Register::R10,
    Register::R11,
    Register::R12,
    Register::R13,
    Register::R14,
    Register::R15,
];

/// Index of RSP in [`State::gpr`].
pub(crate) const RSP: usize = 4;

/// Everything a cave could change in the registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct State {
    /// RAX to R15. On input RSP is ignored: the target always starts on the machine's stack.
    pub(crate) gpr: [u64; 16],
    /// RFLAGS: on input only [`ARITHMETIC_FLAGS`], on output only [`CAPTURED_FLAGS`].
    pub(crate) rflags: u64,
    /// MXCSR, every bit.
    pub(crate) mxcsr: u32,
    /// XMM0 to XMM15, every lane.
    pub(crate) xmm: [u128; 16],
}

impl State {
    /// The value of `register`, a 64-bit general register.
    pub(crate) fn get(&self, register: Register) -> u64 {
        self.gpr[gpr_index(register)]
    }

    /// Sets `register`, a 64-bit general register.
    pub(crate) fn set(&mut self, register: Register, value: u64) {
        self.gpr[gpr_index(register)] = value;
    }
}

fn gpr_index(register: Register) -> usize {
    GPRS.iter()
        .position(|candidate| *candidate == register)
        .unwrap_or_else(|| panic!("{register:?} is not a 64-bit general register"))
}

/// One anonymous mapping standing in for the game's address space.
pub(crate) struct Arena {
    base: NonNull<u8>,
    len: usize,
}

// SAFETY: the arena only hands out copies of its bytes and writes through raw pointers bounded by `len`;
// tests synchronise concurrent writers and executing threads themselves.
unsafe impl Send for Arena {}
// SAFETY: as for `Send`.
unsafe impl Sync for Arena {}

impl Arena {
    /// A read-write mapping of `len` bytes, a multiple of the page size.
    pub(crate) fn new(len: usize) -> Self {
        assert!(len.is_multiple_of(PAGE), "arena length must be whole pages");
        // SAFETY: an anonymous private mapping at an address of the kernel's choosing aliases nothing.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(base, libc::MAP_FAILED, "mmap failed");
        let base = NonNull::new(base.cast::<u8>()).expect("mmap returned null");
        Self { base, len }
    }

    /// The arena's first byte.
    pub(crate) fn base(&self) -> u64 {
        self.base.as_ptr() as u64
    }

    /// The address `offset` bytes in.
    pub(crate) fn at(&self, offset: usize) -> u64 {
        assert!(
            offset <= self.len,
            "offset {offset:#x} is outside the arena"
        );
        self.base() + offset as u64
    }

    fn check(&self, offset: usize, len: usize) {
        let end = offset.checked_add(len).expect("range overflows");
        assert!(
            end <= self.len,
            "range {offset:#x}+{len:#x} is outside the arena"
        );
    }

    /// Copies `bytes` to `offset`, which must lie in read-write pages.
    pub(crate) fn write(&self, offset: usize, bytes: &[u8]) {
        self.check(offset, bytes.len());
        // SAFETY: the range is inside the mapping (checked above) and the source is a separate Rust slice.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.base.as_ptr().wrapping_add(offset),
                bytes.len(),
            );
        }
    }

    /// Writes a little-endian `u64`.
    pub(crate) fn put_u64(&self, offset: usize, value: u64) {
        self.write(offset, &value.to_le_bytes());
    }

    /// Writes a little-endian `u32`.
    pub(crate) fn put_u32(&self, offset: usize, value: u32) {
        self.write(offset, &value.to_le_bytes());
    }

    /// Reads `len` bytes at `offset`.
    pub(crate) fn read(&self, offset: usize, len: usize) -> Vec<u8> {
        self.check(offset, len);
        let mut out = vec![0; len];
        // SAFETY: the range is inside the mapping (checked above) and `out` is a separate allocation.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.base.as_ptr().wrapping_add(offset),
                out.as_mut_ptr(),
                len,
            );
        }
        out
    }

    /// Reads a little-endian `u64`.
    pub(crate) fn get_u64(&self, offset: usize) -> u64 {
        u64::from_le_bytes(self.read(offset, 8).try_into().expect("eight bytes"))
    }

    /// Reads a little-endian `u32`.
    pub(crate) fn get_u32(&self, offset: usize) -> u32 {
        u32::from_le_bytes(self.read(offset, 4).try_into().expect("four bytes"))
    }

    /// The `u32` at `offset`, read atomically while another thread may run code that writes it.
    pub(crate) fn load_u32(&self, offset: usize) -> u32 {
        self.check(offset, 4);
        assert!(offset.is_multiple_of(4), "unaligned atomic");
        // SAFETY: the word is inside the mapping, 4-byte aligned, and other threads access it only with
        // aligned 32-bit moves, which x86-64 performs atomically.
        let word = unsafe {
            std::sync::atomic::AtomicU32::from_ptr(
                self.base.as_ptr().wrapping_add(offset).cast::<u32>(),
            )
        };
        word.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Stores the `u32` at `offset` atomically while another thread may run code that reads it.
    pub(crate) fn store_u32(&self, offset: usize, value: u32) {
        self.check(offset, 4);
        assert!(offset.is_multiple_of(4), "unaligned atomic");
        // SAFETY: as for `load_u32`.
        let word = unsafe {
            std::sync::atomic::AtomicU32::from_ptr(
                self.base.as_ptr().wrapping_add(offset).cast::<u32>(),
            )
        };
        word.store(value, std::sync::atomic::Ordering::SeqCst);
    }

    /// Changes the protection of the pages covering `[offset, offset + len)`.
    pub(crate) fn protect(&self, offset: usize, len: usize, protection: libc::c_int) {
        self.check(offset, len);
        let first = offset / PAGE * PAGE;
        let end = (offset + len).div_ceil(PAGE) * PAGE;
        // SAFETY: the page range lies inside the mapping; changing protection does not move it.
        let status = unsafe {
            libc::mprotect(
                self.base.as_ptr().wrapping_add(first).cast(),
                end - first,
                protection,
            )
        };
        assert_eq!(status, 0, "mprotect failed");
    }

    /// Writes code at `offset`: its pages become read-write for the copy, then read-execute.
    pub(crate) fn code(&self, offset: usize, bytes: &[u8]) {
        self.protect(offset, bytes.len(), libc::PROT_READ | libc::PROT_WRITE);
        self.write(offset, bytes);
        self.protect(offset, bytes.len(), libc::PROT_READ | libc::PROT_EXEC);
    }

    /// Makes the pages covering `[offset, offset + len)` inaccessible.
    pub(crate) fn guard(&self, offset: usize, len: usize) {
        self.protect(offset, len, libc::PROT_NONE);
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        // SAFETY: the mapping is this arena's own and nothing runs in it once the arena is dropped.
        let status = unsafe { libc::munmap(self.base.as_ptr().cast(), self.len) };
        debug_assert_eq!(status, 0, "munmap failed");
    }
}

/// Assembles `emit` at `address`.
pub(crate) fn assemble(
    address: u64,
    emit: impl FnOnce(&mut CodeAssembler) -> Result<(), iced_x86::IcedError>,
) -> Vec<u8> {
    let mut asm = CodeAssembler::new(64).expect("64-bit assembler");
    emit(&mut asm).expect("valid harness code");
    asm.assemble(address).expect("harness code assembles")
}

/// Offsets inside a machine's block.
mod block {
    pub(crate) const IN: usize = 0;
    pub(crate) const OUT: usize = 0x200;
    pub(crate) const GPR: usize = 0;
    pub(crate) const RFLAGS: usize = 0x80;
    pub(crate) const MXCSR: usize = 0x88;
    pub(crate) const XMM: usize = 0x90;
    pub(crate) const HOST_RSP: usize = 0x400;
    pub(crate) const ENTRY_RSP: usize = 0x408;
}

/// A generated function that runs a target from a [`State`] and captures the [`State`] it returns with.
pub(crate) struct Machine {
    code: usize,
    block: usize,
    entry: usize,
    capture: u64,
}

impl Machine {
    /// A machine with its code at `code` (one page), its state block at `block` (read-write) and a stack
    /// whose target entry RSP is `entry`. The stack below `entry` must be read-write.
    pub(crate) fn new(arena: &Arena, code: usize, block: usize, entry: usize) -> Self {
        assert!(
            entry % 16 == 8,
            "a called function starts with RSP 8 below a 16-byte boundary"
        );
        let at = |offset| arena.at(block + offset);
        let bytes = assemble(arena.at(code), |a| {
            // Host callee-saved registers and MXCSR.
            for register in [rbx, rbp, r12, r13, r14, r15] {
                a.push(register)?;
            }
            a.sub(rsp, 8)?;
            a.stmxcsr(dword_ptr(rsp))?;
            a.mov(rax, at(0))?;
            a.mov(qword_ptr(rax + block::HOST_RSP as i32), rsp)?;
            for (index, register) in xmm_registers() {
                a.movdqu(
                    register,
                    xmmword_ptr(rax + (block::IN + block::XMM + 16 * index) as i32),
                )?;
            }
            a.ldmxcsr(dword_ptr(rax + (block::IN + block::MXCSR) as i32))?;
            // [entry - 8] holds the target and [entry] the capture address: `ret` enters the target as if
            // called.
            a.mov(rsp, qword_ptr(rax + block::ENTRY_RSP as i32))?;
            a.push(qword_ptr(rax + (block::IN + block::RFLAGS) as i32))?;
            a.popfq()?;
            for (index, register) in gpr_registers().filter(|(_, r)| *r != rax) {
                a.mov(
                    register,
                    qword_ptr(rax + (block::IN + block::GPR + 8 * index) as i32),
                )?;
            }
            a.mov(rax, qword_ptr(rax + (block::IN + block::GPR) as i32))?;
            a.ret()?;
            Ok(())
        });
        let capture_at = arena.at(code) + bytes.len() as u64;
        let capture = assemble(capture_at, |a| {
            a.pushfq()?;
            a.push(rax)?;
            a.mov(rax, at(0))?;
            a.pop(qword_ptr(rax + (block::OUT + block::GPR) as i32))?;
            a.pop(qword_ptr(rax + (block::OUT + block::RFLAGS) as i32))?;
            a.mov(
                qword_ptr(rax + (block::OUT + block::GPR + 8 * RSP) as i32),
                rsp,
            )?;
            for (index, register) in gpr_registers().filter(|(_, r)| *r != rax) {
                a.mov(
                    qword_ptr(rax + (block::OUT + block::GPR + 8 * index) as i32),
                    register,
                )?;
            }
            for (index, register) in xmm_registers() {
                a.movdqu(
                    xmmword_ptr(rax + (block::OUT + block::XMM + 16 * index) as i32),
                    register,
                )?;
            }
            a.stmxcsr(dword_ptr(rax + (block::OUT + block::MXCSR) as i32))?;
            a.mov(rsp, qword_ptr(rax + block::HOST_RSP as i32))?;
            a.ldmxcsr(dword_ptr(rsp))?;
            a.add(rsp, 8)?;
            for register in [r15, r14, r13, r12, rbp, rbx] {
                a.pop(register)?;
            }
            a.cld()?;
            a.ret()?;
            Ok(())
        });
        let mut all = bytes;
        all.extend_from_slice(&capture);
        assert!(all.len() <= PAGE, "machine exceeds a page");
        arena.code(code, &all);
        arena.put_u64(block + block::ENTRY_RSP, arena.at(entry) - 8);
        Self {
            code,
            block,
            entry,
            capture: capture_at,
        }
    }

    /// The RSP a target starts with.
    pub(crate) fn entry(&self, arena: &Arena) -> u64 {
        arena.at(self.entry)
    }

    /// Runs `target` from `input` and returns the state it returned with.
    pub(crate) fn run(&self, arena: &Arena, target: u64, input: &State) -> State {
        assert_eq!(
            input.rflags & !ARITHMETIC_FLAGS,
            0,
            "seed flags outside the arithmetic set"
        );
        self.enter(arena, target, input);
        self.load(arena, block::OUT)
    }

    /// Runs `target` from `input` without allocating, so a forked child can call it.
    pub(crate) fn enter(&self, arena: &Arena, target: u64, input: &State) {
        self.store(arena, block::IN, input);
        // The capture code reuses these two slots, so every run writes them again.
        arena.put_u64(self.entry - 8, target);
        arena.put_u64(self.entry, self.capture);
        let entry = arena.at(self.code);
        // SAFETY: `entry` is this machine's generated code, a System V function taking nothing and returning
        // nothing that restores every callee-saved register, the stack pointer, MXCSR and the direction flag.
        // The target is test code or a cave the test placed in the arena.
        let function: extern "sysv64" fn() = unsafe { std::mem::transmute(entry as usize) };
        function();
    }

    fn store(&self, arena: &Arena, at: usize, state: &State) {
        let base = self.block + at;
        for (index, value) in state.gpr.iter().enumerate() {
            arena.put_u64(base + block::GPR + 8 * index, *value);
        }
        arena.put_u64(base + block::RFLAGS, state.rflags | 2);
        arena.put_u32(base + block::MXCSR, state.mxcsr);
        for (index, value) in state.xmm.iter().enumerate() {
            arena.write(base + block::XMM + 16 * index, &value.to_le_bytes());
        }
    }

    fn load(&self, arena: &Arena, at: usize) -> State {
        let base = self.block + at;
        let mut state = State {
            gpr: [0; 16],
            rflags: arena.get_u64(base + block::RFLAGS) & CAPTURED_FLAGS,
            mxcsr: arena.get_u32(base + block::MXCSR),
            xmm: [0; 16],
        };
        for index in 0..16 {
            state.gpr[index] = arena.get_u64(base + block::GPR + 8 * index);
            let lanes = arena.read(base + block::XMM + 16 * index, 16);
            state.xmm[index] = u128::from_le_bytes(lanes.try_into().expect("sixteen bytes"));
        }
        state
    }
}

fn gpr_registers() -> impl Iterator<Item = (usize, iced_x86::code_asm::AsmRegister64)> {
    GPRS.iter()
        .enumerate()
        .filter(|(_, register)| **register != Register::RSP)
        .map(|(index, register)| (index, get_gpr64(*register).expect("general register")))
}

fn xmm_registers() -> impl Iterator<Item = (usize, iced_x86::code_asm::AsmRegisterXmm)> {
    [
        xmm0, xmm1, xmm2, xmm3, xmm4, xmm5, xmm6, xmm7, xmm8, xmm9, xmm10, xmm11, xmm12, xmm13,
        xmm14, xmm15,
    ]
    .into_iter()
    .enumerate()
}

/// How a forked child ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    /// It exited with this code.
    Exited(i32),
    /// A signal killed it.
    Signalled(i32),
}

/// Runs `child` in a forked copy of this process and reports how it ended. The child exits with the code
/// `child` returns; it must not allocate, take locks or touch anything another thread may hold.
pub(crate) fn forked(child: impl FnOnce() -> i32) -> Ending {
    // SAFETY: the child only runs `child`, which by contract avoids allocation and locks, then `_exit`s
    // without running destructors or atexit handlers.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed");
    if pid == 0 {
        let code = child();
        // SAFETY: ends the child at once, as `fork`'s contract requires.
        unsafe { libc::_exit(code) };
    }
    let mut status = 0;
    // SAFETY: waits for the child just forked; `status` is a valid out-pointer.
    let waited = unsafe { libc::waitpid(pid, &raw mut status, 0) };
    assert_eq!(waited, pid, "waitpid failed");
    if libc::WIFEXITED(status) {
        Ending::Exited(libc::WEXITSTATUS(status))
    } else {
        Ending::Signalled(libc::WTERMSIG(status))
    }
}
