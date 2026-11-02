// SPDX-License-Identifier: AGPL-3.0-only
//! No-reload caves: keep a selected class's loaded rounds when the player's own weapon fires.
//!
//! The prologue saves the flags, RAX, RCX, RDX and R8, reads the control word once into R8D, and takes the
//! original path at once unless some class bit is set, so the disabled path dereferences nothing. Otherwise it
//! classifies the weapon in the shape's receiver register: its owner must be the player, reached through the
//! profile's pointer chain with every link checked for null, and its configuration's first DWORD must equal
//! one of a class's interned ids. A selected class takes the selected path; anything else, including another
//! actor's weapon or an unknown class, takes the original path. Both paths restore the five saved values first.
#![forbid(unsafe_code)]

use iced_x86::code_asm::{
    AsmRegister64, CodeLabel, dword_ptr, eax, get_gpr64, qword_ptr, r8, r8d, rax, rcx, rdx,
};
use iced_x86::{Code, Instruction, Mnemonic, OpKind, Register};

use crate::addr::Va;
use crate::control::ControlSet;
use crate::profile::{Profile, ReloadKind, Site};

use super::emit::{self, Draft, Emitter, Fault};
use super::{Exit, Layout};

/// Builds the no-reload cave for `site`, hooked at `at`.
pub(super) fn build(
    site: &Site,
    profile: &Profile,
    layout: Layout,
    at: Va,
    kind: ReloadKind,
) -> Result<Draft, Fault> {
    let original = emit::decode(site.original, at)?;
    check_calls(&original, kind)?;
    let selected = selected_path(&original, kind)?;
    let cont = at.checked_add(site.original.len() as u64)?;
    let receiver = receiver(kind)?;
    let player = &profile.player;
    let offset = |value, what| emit::imm32(value, what);
    let weapon_owner = offset(player.weapon_owner, "the weapon owner offset is past 2 GiB")?;
    let weapon_config = offset(
        player.weapon_config,
        "the weapon config offset is past 2 GiB",
    )?;
    let player_offset = offset(player.player, "the player offset is past 2 GiB")?;
    let owner = offset(player.owner, "the owner offset is past 2 GiB")?;
    let manager = layout.image().at(player.manager)?;
    let any_class = profile
        .classes
        .iter()
        .fold(ControlSet::EMPTY, |bits, class| bits.with(class.bit));

    let mut e = Emitter::new()?;
    let mut ordinary = e.label();
    let mut chosen = e.label();
    e.asm().pushfq()?;
    for register in [rax, rcx, rdx, r8] {
        e.asm().push(register)?;
    }
    e.asm().mov(rax, layout.controls().get())?;
    e.asm().mov(r8d, dword_ptr(rax))?;
    e.asm().test(r8d, any_class.word())?;
    e.asm().jz(ordinary)?;
    // The weapon's owner must be the player.
    e.asm().mov(rax, qword_ptr(receiver + weapon_owner))?;
    nonzero(&mut e, rax, ordinary)?;
    e.asm().mov(rdx, qword_ptr(rax))?;
    nonzero(&mut e, rdx, ordinary)?;
    e.asm().mov(rax, manager.get())?;
    e.asm().mov(rax, qword_ptr(rax))?;
    nonzero(&mut e, rax, ordinary)?;
    e.asm().mov(rax, qword_ptr(rax + player_offset))?;
    nonzero(&mut e, rax, ordinary)?;
    e.asm().add(rax, owner)?;
    e.asm().cmp(rdx, rax)?;
    e.asm().jne(ordinary)?;
    // Its configuration's first DWORD is the interned class id.
    e.asm().mov(rax, qword_ptr(receiver + weapon_config))?;
    nonzero(&mut e, rax, ordinary)?;
    e.asm().mov(eax, dword_ptr(rax))?;
    let mut matched: Vec<CodeLabel> = profile.classes.iter().map(|_| e.label()).collect();
    for (class, label) in profile.classes.iter().zip(&matched) {
        for &id in class.interned_ids {
            e.asm().mov(rcx, layout.image().at(id)?.get())?;
            e.asm().cmp(eax, dword_ptr(rcx))?;
            e.asm().je(*label)?;
        }
    }
    e.asm().jmp(ordinary)?;
    for (class, label) in profile.classes.iter().zip(&mut matched) {
        e.bind(label)?;
        e.asm().test(r8d, class.bit.word())?;
        e.asm().jnz(chosen)?;
        e.asm().jmp(ordinary)?;
    }
    e.bind(&mut ordinary)?;
    restore(&mut e)?;
    e.replay(&original)?;
    e.jump(cont)?;
    e.bind(&mut chosen)?;
    restore(&mut e)?;
    e.replay(&selected)?;
    e.jump(cont)?;
    e.finish(layout.cave(), vec![Exit::Jump(cont)])
}

/// The register holding the weapon at the site.
fn receiver(kind: ReloadKind) -> Result<AsmRegister64, Fault> {
    let register = match kind {
        ReloadKind::SharedR14 => Register::R14,
        ReloadKind::DirectCallbackRcx | ReloadKind::EventCallbackRcx => Register::RCX,
        ReloadKind::HellsingFireRdi => Register::RDI,
    };
    get_gpr64(register).ok_or(Fault::Unencodable("receiver register"))
}

/// Only the direct callback holds a call: an indirect call through a register-based memory operand, which
/// returns into the cave. Everything else must be position-independent.
fn check_calls(original: &[Instruction], kind: ReloadKind) -> Result<(), Fault> {
    let mut calls = 0;
    for instruction in original {
        if emit::indirect_call(instruction) {
            calls += 1;
        } else if !emit::position_independent(instruction) {
            return Err(Fault::Original(
                "it holds a branch, call or RIP-relative operand the hook does not handle",
            ));
        }
    }
    match (kind.returns_into_cave(), calls) {
        (true, 1) | (false, 0) => Ok(()),
        _ => Err(Fault::Original(
            "its calls do not match whether the game returns into the cave",
        )),
    }
}

/// The selected path's instructions: the original with the shot's round count kept.
///
/// The shared fire routine drops the `sub` that spends the round. The other shapes compute the new count with
/// `lea edx,[rax-1]`; the selected path passes the current count instead, with `mov edx,eax`. Neither changes
/// the flags.
fn selected_path(original: &[Instruction], kind: ReloadKind) -> Result<Vec<Instruction>, Fault> {
    if kind == ReloadKind::SharedR14 {
        return match original {
            [load, spend]
                if load.mnemonic() == Mnemonic::Mov
                    && load.op1_kind() == OpKind::Memory
                    && spend.mnemonic() == Mnemonic::Sub =>
            {
                Ok(vec![*load])
            }
            _ => Err(Fault::Original(
                "it is not a load followed by a subtraction",
            )),
        };
    }
    let decrement = |instruction: &Instruction| {
        instruction.mnemonic() == Mnemonic::Lea
            && instruction.op0_register() == Register::EDX
            && instruction.memory_base() == Register::RAX
            && instruction.memory_index() == Register::None
            && instruction.memory_displacement64().cast_signed() == -1
    };
    if original
        .iter()
        .filter(|instruction| decrement(instruction))
        .count()
        != 1
    {
        return Err(Fault::Original(
            "it does not hold exactly one `lea edx,[rax-1]`",
        ));
    }
    let keep = Instruction::with2(Code::Mov_r32_rm32, Register::EDX, Register::EAX)?;
    Ok(original
        .iter()
        .map(|instruction| {
            if decrement(instruction) {
                keep
            } else {
                *instruction
            }
        })
        .collect())
}

/// `test register, register; jz to`.
fn nonzero(e: &mut Emitter, register: AsmRegister64, to: CodeLabel) -> Result<(), Fault> {
    e.asm().test(register, register)?;
    Ok(e.asm().jz(to)?)
}

/// Each path's start: restores R8, RDX, RCX, RAX and the flags.
fn restore(e: &mut Emitter) -> Result<(), Fault> {
    for register in [r8, rdx, rcx, rax] {
        e.asm().pop(register)?;
    }
    Ok(e.asm().popfq()?)
}
