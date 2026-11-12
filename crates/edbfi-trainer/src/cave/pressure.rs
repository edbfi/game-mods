// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pressure caves: keep the pressure class's pressure at its normal level, for the player's weapon only.
//!
//! A consumption site runs its original first, under the game's own MXCSR, then corrects the result register;
//! the recovery site corrects the stored pressure before its original prologue. The correction saves the
//! flags, RAX, RDX and R11 and reserves a 32-byte frame holding all of XMM2 and the caller's MXCSR. It loads a
//! copy of MXCSR with every exception masked, so its comparisons never trap, while keeping DAZ, FTZ and the
//! rounding mode; restoring the saved MXCSR afterwards also drops any sticky flag the correction raised.
//!
//! The gate order is: the control bit; the owner chain, each link checked for null, ending at the player; a
//! non-null configuration; the pressure class's interned id; then a normal pressure whose bits are a positive,
//! finite, non-zero float (denormals allowed). Only then `ucomiss normal, pressure`: unordered or
//! `normal <= pressure` leaves the pressure alone, otherwise it becomes `normal`, which floors even −∞.
#![forbid(unsafe_code)]

use iced_x86::code_asm::{
    AsmRegisterXmm, dword_ptr, eax, get_xmm, qword_ptr, r11, rax, rbx, rcx, rdx, rsp, xmm2,
    xmmword_ptr,
};
use iced_x86::{Instruction, InstructionInfoFactory, OpAccess, OpKind, Register};

use crate::addr::Va;
use crate::control::ControlBit;
use crate::profile::{PressureKind, Profile, Site};

use super::emit::{self, Draft, Emitter, Fault};
use super::{Exit, Layout};

/// The scratch frame: XMM2 at `[rsp]`, the caller's MXCSR at `[rsp+0x10]`, the masked copy at `[rsp+0x14]`.
const FRAME: i32 = 0x20;
/// Where the caller's MXCSR is saved.
const SAVED_MXCSR: i32 = 0x10;
/// Where the masked copy is built.
const MASKED_MXCSR: i32 = 0x14;
/// MXCSR's six exception-mask bits.
const MASK_ALL: i32 = 0x1F80;
/// The bits of +∞: a normal pressure must be below it.
const INFINITY_BITS: u32 = 0x7F80_0000;

/// What the correction floors.
enum Floor {
    /// The weapon's stored pressure.
    Stored,
    /// A register holding the consumption's result.
    Result(AsmRegisterXmm),
}

/// Builds the pressure cave for `site`, hooked at `at`.
pub(super) fn build(
    site: &Site,
    profile: &Profile,
    layout: Layout,
    at: Va,
    bit: ControlBit,
    kind: PressureKind,
) -> Result<Draft, Fault> {
    let original = emit::decode(site.original, at)?;
    emit::require_position_independent(&original)?;
    let (weapon, floor) = match kind {
        PressureKind::RecoveryRcx => (rcx, Floor::Stored),
        PressureKind::Consumption1RbxXmm1 => {
            (rbx, Floor::Result(result(&original, Register::XMM1)?))
        }
        PressureKind::Consumption2RbxXmm0 => {
            (rbx, Floor::Result(result(&original, Register::XMM0)?))
        }
    };
    if matches!(floor, Floor::Result(_)) && writes(&original, Register::RBX) {
        // The consumption runs first, so it must leave the weapon where the cave reads it.
        return Err(Fault::Original("it overwrites the weapon register"));
    }
    let cont = at.checked_add(site.original.len() as u64)?;
    let player = &profile.player;
    let class = profile
        .classes
        .iter()
        .find(|class| class.key == player.pressure_class)
        .ok_or(Fault::Unencodable("the pressure class is not a class"))?;
    let offset = |value, what| emit::imm32(value, what);
    let weapon_owner = offset(player.weapon_owner, "the weapon owner offset is past 2 GiB")?;
    let weapon_config = offset(
        player.weapon_config,
        "the weapon config offset is past 2 GiB",
    )?;
    let pressure = offset(
        player.weapon_pressure,
        "the weapon pressure offset is past 2 GiB",
    )?;
    let normal = offset(
        player.config_normal_pressure,
        "the normal pressure offset is past 2 GiB",
    )?;
    let player_offset = offset(player.player, "the player offset is past 2 GiB")?;
    let owner = offset(player.owner, "the owner offset is past 2 GiB")?;
    let manager = layout.image().at(player.manager)?;

    let mut e = Emitter::new()?;
    let mut done = e.label();
    let mut is_class = e.label();
    if matches!(floor, Floor::Result(_)) {
        e.replay(&original)?;
    }
    e.asm().pushfq()?;
    for register in [rax, rdx, r11] {
        e.asm().push(register)?;
    }
    e.asm().sub(rsp, FRAME)?;
    e.asm().stmxcsr(dword_ptr(rsp + SAVED_MXCSR))?;
    e.asm().movdqu(xmmword_ptr(rsp), xmm2)?;
    e.asm().mov(eax, dword_ptr(rsp + SAVED_MXCSR))?;
    e.asm().or(eax, MASK_ALL)?;
    e.asm().mov(dword_ptr(rsp + MASKED_MXCSR), eax)?;
    e.asm().ldmxcsr(dword_ptr(rsp + MASKED_MXCSR))?;
    e.asm().mov(r11, layout.controls().get())?;
    e.asm().mov(eax, dword_ptr(r11))?;
    e.asm().test(eax, bit.word())?;
    e.asm().jz(done)?;
    // The weapon's owner must be the player.
    e.asm().mov(r11, weapon)?;
    e.asm().mov(rax, qword_ptr(r11 + weapon_owner))?;
    e.asm().test(rax, rax)?;
    e.asm().jz(done)?;
    e.asm().mov(rdx, qword_ptr(rax))?;
    e.asm().test(rdx, rdx)?;
    e.asm().jz(done)?;
    e.asm().mov(rax, manager.get())?;
    e.asm().mov(rax, qword_ptr(rax))?;
    e.asm().test(rax, rax)?;
    e.asm().jz(done)?;
    e.asm().mov(rax, qword_ptr(rax + player_offset))?;
    e.asm().test(rax, rax)?;
    e.asm().jz(done)?;
    e.asm().add(rax, owner)?;
    e.asm().cmp(rdx, rax)?;
    e.asm().jne(done)?;
    // Its configuration must be the pressure class's.
    e.asm().mov(rdx, qword_ptr(r11 + weapon_config))?;
    e.asm().test(rdx, rdx)?;
    e.asm().jz(done)?;
    for &id in class.interned_ids {
        e.asm().mov(rax, layout.image().at(id)?.get())?;
        e.asm().mov(eax, dword_ptr(rax))?;
        e.asm().cmp(dword_ptr(rdx), eax)?;
        e.asm().je(is_class)?;
    }
    e.asm().jmp(done)?;
    e.bind(&mut is_class)?;
    // The normal pressure must be a positive, finite, non-zero float.
    e.asm().mov(eax, dword_ptr(rdx + normal))?;
    e.asm().test(eax, eax)?;
    e.asm().jle(done)?;
    e.asm().cmp(eax, INFINITY_BITS)?;
    e.asm().jae(done)?;
    // Compare the value just checked, not a second read of it.
    e.asm().movd(xmm2, eax)?;
    match floor {
        Floor::Stored => {
            e.asm().ucomiss(xmm2, dword_ptr(r11 + pressure))?;
            e.asm().jp(done)?;
            e.asm().jbe(done)?;
            e.effect(|asm| asm.movss(dword_ptr(r11 + pressure), xmm2))?;
        }
        Floor::Result(register) => {
            e.asm().ucomiss(xmm2, register)?;
            e.asm().jp(done)?;
            e.asm().jbe(done)?;
            e.effect(|asm| asm.movss(register, xmm2))?;
        }
    }
    e.bind(&mut done)?;
    e.asm().ldmxcsr(dword_ptr(rsp + SAVED_MXCSR))?;
    e.asm().movdqu(xmm2, xmmword_ptr(rsp))?;
    e.asm().add(rsp, FRAME)?;
    for register in [r11, rdx, rax] {
        e.asm().pop(register)?;
    }
    e.asm().popfq()?;
    if matches!(floor, Floor::Stored) {
        e.replay(&original)?;
    }
    e.jump(cont)?;
    e.finish(layout.cave(), vec![Exit::Jump(cont)])
}

/// Whether any of `instructions` writes `register` or part of it.
fn writes(instructions: &[Instruction], register: Register) -> bool {
    let mut info = InstructionInfoFactory::new();
    instructions.iter().any(|instruction| {
        info.info(instruction).used_registers().iter().any(|used| {
            used.register().full_register() == register
                && matches!(
                    used.access(),
                    OpAccess::Write
                        | OpAccess::CondWrite
                        | OpAccess::ReadWrite
                        | OpAccess::ReadCondWrite
                )
        })
    })
}

/// The register a consumption original leaves its result in: its last instruction must write `register`.
fn result(original: &[iced_x86::Instruction], register: Register) -> Result<AsmRegisterXmm, Fault> {
    let last = original
        .last()
        .filter(|last| last.op0_kind() == OpKind::Register && last.op0_register() == register)
        .ok_or(Fault::Original(
            "its last instruction does not write the result register",
        ))?;
    get_xmm(last.op0_register()).ok_or(Fault::Original("the result is not an XMM register"))
}
