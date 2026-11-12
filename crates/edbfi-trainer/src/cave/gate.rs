// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate caves: run the original unless the site's control bit is set, in which case do the gate's effect.
//!
//! The prologue saves the flags and RAX, reads the control word once and tests the bit. Each path starts by
//! restoring RAX and the flags, so it begins with RAX, RSP and the flags as they were at the site. The
//! scaffolding touches no other register and no SIMD state; only an effect does, such as zeroing XMM9.
#![forbid(unsafe_code)]

use iced_x86::code_asm::{al, dword_ptr, eax, rax, xmm9};
use iced_x86::{Code, FlowControl, Instruction, Mnemonic, OpKind, Register};

use crate::addr::{Rva, Va};
use crate::control::ControlBit;
use crate::profile::{GateEffect, Site};

use super::emit::{self, Draft, Emitter, Fault};
use super::{Callee, Exit, Layout};

/// Builds the gate cave for `site`, hooked at `at`.
pub(super) fn build(
    site: &Site,
    layout: Layout,
    at: Va,
    bit: ControlBit,
    effect: GateEffect,
) -> Result<Draft, Fault> {
    let original = emit::decode(site.original, at)?;
    let cont = at.checked_add(site.original.len() as u64)?;
    let target = |rva: Rva| layout.image().at(rva);
    let mut e = Emitter::new()?;
    let mut enabled = e.label();
    e.asm().pushfq()?;
    e.asm().push(rax)?;
    e.asm().mov(rax, layout.controls().get())?;
    e.asm().mov(eax, dword_ptr(rax))?;
    e.asm().test(eax, bit.word())?;
    e.asm().jnz(enabled)?;
    restore(&mut e)?;
    let exits = match effect {
        GateEffect::ReturnTrue => {
            emit::require_position_independent(&original)?;
            e.replay(&original)?;
            e.jump(cont)?;
            e.bind(&mut enabled)?;
            restore(&mut e)?;
            e.game(|asm| {
                asm.mov(al, 1)?;
                asm.ret()
            })?;
            vec![Exit::Jump(cont), Exit::Return]
        }
        GateEffect::ReserveLoad => {
            emit::require_position_independent(&original)?;
            let [load, subtract] = original.as_slice() else {
                return Err(Fault::Original(
                    "it is not a load followed by a subtraction",
                ));
            };
            if load.mnemonic() != Mnemonic::Mov
                || load.op1_kind() != OpKind::Memory
                || subtract.mnemonic() != Mnemonic::Sub
            {
                return Err(Fault::Original(
                    "it is not a load followed by a subtraction",
                ));
            }
            e.replay(&original)?;
            e.jump(cont)?;
            e.bind(&mut enabled)?;
            restore(&mut e)?;
            e.replay(&original[..1])?;
            e.jump(cont)?;
            vec![Exit::Jump(cont)]
        }
        GateEffect::Throwable {
            callee,
            enabled: action,
        } => {
            let (callee, action) = (target(callee)?, target(action)?);
            direct_call(&original, callee)?;
            e.tail_call(callee, cont)?;
            e.bind(&mut enabled)?;
            restore(&mut e)?;
            e.tail_call(action, cont)?;
            vec![
                Exit::TailCall {
                    callee: Callee::Fixed(callee),
                    ret: cont,
                },
                Exit::TailCall {
                    callee: Callee::Fixed(action),
                    ret: cont,
                },
            ]
        }
        GateEffect::ZeroXmm9 => {
            emit::require_position_independent(&original)?;
            let only = emit::single(&original)?;
            if only.op0_kind() != OpKind::Register || only.op0_register() != Register::XMM9 {
                return Err(Fault::Original("it does not write xmm9"));
            }
            e.replay(&original)?;
            e.jump(cont)?;
            e.bind(&mut enabled)?;
            restore(&mut e)?;
            e.game(|asm| asm.xorps(xmm9, xmm9))?;
            e.jump(cont)?;
            vec![Exit::Jump(cont)]
        }
        GateEffect::SkipMaskCall => {
            let call = emit::single(&original)?;
            e.tail_call_via(call, cont)?;
            e.bind(&mut enabled)?;
            restore(&mut e)?;
            e.jump(cont)?;
            vec![
                Exit::TailCall {
                    callee: Callee::Indirect,
                    ret: cont,
                },
                Exit::Jump(cont),
            ]
        }
        GateEffect::SkipMedkitAnd => {
            emit::require_position_independent(&original)?;
            if emit::single(&original)?.mnemonic() != Mnemonic::And {
                return Err(Fault::Original("it is not an `and`"));
            }
            e.replay(&original)?;
            e.jump(cont)?;
            e.bind(&mut enabled)?;
            restore(&mut e)?;
            e.jump(cont)?;
            vec![Exit::Jump(cont)]
        }
        GateEffect::GodBranch { target: branch } => {
            let branch = target(branch)?;
            emit::require_position_independent(&original)?;
            if emit::single(&original)?.mnemonic() != Mnemonic::Cmp {
                return Err(Fault::Original("it is not a `cmp`"));
            }
            e.replay(&original)?;
            e.jump(cont)?;
            e.bind(&mut enabled)?;
            restore(&mut e)?;
            e.jump(branch)?;
            vec![Exit::Jump(cont), Exit::Jump(branch)]
        }
        GateEffect::SkipBatteryCall { callee } => {
            let callee = target(callee)?;
            direct_call(&original, callee)?;
            e.tail_call(callee, cont)?;
            e.bind(&mut enabled)?;
            restore(&mut e)?;
            e.jump(cont)?;
            vec![
                Exit::TailCall {
                    callee: Callee::Fixed(callee),
                    ret: cont,
                },
                Exit::Jump(cont),
            ]
        }
    };
    e.finish(layout.cave(), exits)
}

/// Each path's start: `pop rax; popfq`.
fn restore(e: &mut Emitter) -> Result<(), Fault> {
    e.asm().pop(rax)?;
    Ok(e.asm().popfq()?)
}

/// Checks that the original is exactly `call callee`.
fn direct_call(original: &[Instruction], callee: Va) -> Result<(), Fault> {
    let call = emit::single(original)?;
    let direct = call.code() == Code::Call_rel32_64 && call.flow_control() == FlowControl::Call;
    if direct && call.near_branch_target() == callee.get() {
        Ok(())
    } else {
        Err(Fault::Original(
            "it is not a direct call to the reviewed callee",
        ))
    }
}
