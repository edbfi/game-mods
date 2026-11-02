// SPDX-License-Identifier: AGPL-3.0-only
#![allow(clippy::unwrap_used, clippy::expect_used)]

use iced_x86::Register;
use iced_x86::code_asm::{
    dword_ptr, eax, edx, esp, ptr, qword_ptr, r8, rax, rbx, rcx, rdx, rsp, xmm2,
};
use proptest::prelude::*;

use super::emit::{Draft, Emitter, Fault};
use super::place::{FreeRegion, Reach, candidates};
use super::*;
use crate::addr::Rva;
use crate::control::ControlBit;
use crate::profile::synthetic::SYNTHETIC;
use crate::profile::{GateEffect, PressureKind, ReloadKind};

const IMAGE: Va = Va::new(0x1_4000_0000);
const CAVE: Va = Va::new(0x1_3000_0000);
const CONTROLS: Va = Va::new(0x1_2000_0000);

fn layout() -> Layout {
    Layout::new(IMAGE, CAVE, CONTROLS).unwrap()
}

/// A site of `kind` at `rva` with `original`; its context is irrelevant to building.
fn site(rva: u32, original: &'static [u8], kind: HookKind) -> Site {
    Site {
        id: SiteId(9),
        rva: Rva::new(rva),
        original,
        context: SYNTHETIC.sites[0].context,
        kind,
    }
}

fn gate(rva: u32, original: &'static [u8], effect: GateEffect) -> Site {
    site(
        rva,
        original,
        HookKind::Gate {
            bit: ControlBit::new(1),
            effect,
        },
    )
}

fn built(site: &Site) -> Result<Built, HookError> {
    build(site, &SYNTHETIC, layout())
}

fn cont(site: &Site) -> Va {
    IMAGE
        .at(site.rva)
        .unwrap()
        .checked_add(site.original.len() as u64)
        .unwrap()
}

#[test]
fn the_synthetic_profile_plans_one_cave_per_page() {
    let valid = ValidProfile::new(&SYNTHETIC).unwrap();
    let plan = plan(&valid, IMAGE, CAVE, CONTROLS).unwrap();
    assert_eq!(plan.allocation(), CAVE);
    assert_eq!(plan.hooks().len(), SYNTHETIC.sites.len());
    assert!(!plan.returns_into_cave());
    for (page, hook) in plan.hooks().iter().enumerate() {
        assert_eq!(
            hook.cave().address(),
            CAVE.checked_add(page as u64 * u64::from(PAGE)).unwrap()
        );
        assert_eq!(hook.jump().len(), hook.original().len());
        assert_eq!(hook.range().start(), hook.at());
        assert!(hook.cave().range().len().get() <= PAGE);
    }
    assert_eq!(allocation_len(&SYNTHETIC), 4 * u64::from(PAGE));
}

#[test]
fn every_gate_effect_builds_with_its_exits() {
    let callee = Rva::new(0x2000);
    let enabled = Rva::new(0x2030);
    let target = Rva::new(0x1F00);
    let at = |rva: Rva| IMAGE.at(rva).unwrap();
    let cases: [(Site, Vec<Exit>); 8] = [
        (
            gate(
                0x1000,
                crate::bytes!("48 89 5C 24 08"),
                GateEffect::ReturnTrue,
            ),
            vec![Exit::Return],
        ),
        (
            gate(
                0x1100,
                crate::bytes!("49 8B 06 2B DF"),
                GateEffect::ReserveLoad,
            ),
            vec![],
        ),
        (
            // call 0x2000 from 0x1200: rel32 = 0x2000 - 0x1205.
            gate(
                0x1200,
                crate::bytes!("E8 FB 0D 00 00"),
                GateEffect::Throwable { callee, enabled },
            ),
            vec![],
        ),
        (
            gate(
                0x1300,
                crate::bytes!("F3 44 0F 59 CA"),
                GateEffect::ZeroXmm9,
            ),
            vec![],
        ),
        (
            gate(
                0x1400,
                crate::bytes!("FF 90 B0 01 00 00"),
                GateEffect::SkipMaskCall,
            ),
            vec![],
        ),
        (
            gate(
                0x1500,
                crate::bytes!("41 21 80 78 04 00 00"),
                GateEffect::SkipMedkitAnd,
            ),
            vec![],
        ),
        (
            gate(
                0x1600,
                crate::bytes!("80 B9 44 13 00 00 02"),
                GateEffect::GodBranch { target },
            ),
            vec![],
        ),
        (
            gate(
                0x1700,
                crate::bytes!("E8 FB 08 00 00"),
                GateEffect::SkipBatteryCall { callee },
            ),
            vec![],
        ),
    ];
    for (site, mut extra) in cases {
        let cave = built(&site).unwrap();
        let cont = cont(&site);
        let mut want = match site.kind {
            HookKind::Gate {
                effect: GateEffect::Throwable { .. },
                ..
            } => vec![
                Exit::TailCall {
                    callee: Callee::Fixed(at(callee)),
                    ret: cont,
                },
                Exit::TailCall {
                    callee: Callee::Fixed(at(enabled)),
                    ret: cont,
                },
            ],
            HookKind::Gate {
                effect: GateEffect::SkipMaskCall,
                ..
            } => vec![
                Exit::TailCall {
                    callee: Callee::Indirect,
                    ret: cont,
                },
                Exit::Jump(cont),
            ],
            HookKind::Gate {
                effect: GateEffect::SkipBatteryCall { .. },
                ..
            } => vec![
                Exit::TailCall {
                    callee: Callee::Fixed(at(callee)),
                    ret: cont,
                },
                Exit::Jump(cont),
            ],
            HookKind::Gate {
                effect: GateEffect::GodBranch { .. },
                ..
            } => vec![Exit::Jump(cont), Exit::Jump(at(target))],
            _ => vec![Exit::Jump(cont)],
        };
        want.append(&mut extra);
        want.sort_unstable();
        assert_eq!(cave.exits(), want, "{:?}", site.kind);
        assert!(!cave.returns_into_cave());
    }
}

#[test]
fn every_reload_and_pressure_shape_builds() {
    let reload = |rva, original, kind| site(rva, original, HookKind::NoReload(kind));
    let pressure = |rva, original, kind| {
        site(
            rva,
            original,
            HookKind::Pressure {
                bit: ControlBit::new(31),
                kind,
            },
        )
    };
    let cases = [
        reload(
            0x1000,
            crate::bytes!("49 8B 06 41 2B FD"),
            ReloadKind::SharedR14,
        ),
        reload(
            0x1100,
            crate::bytes!("8D 50 FF FF 93 60 13 00 00"),
            ReloadKind::DirectCallbackRcx,
        ),
        reload(
            0x1200,
            crate::bytes!("8D 50 FF 48 8B 46 F8"),
            ReloadKind::EventCallbackRcx,
        ),
        reload(
            0x1300,
            crate::bytes!("48 8B CF 8D 50 FF"),
            ReloadKind::HellsingFireRdi,
        ),
        pressure(
            0x1400,
            crate::bytes!("40 53 48 83 EC 20"),
            PressureKind::RecoveryRcx,
        ),
        pressure(
            0x1500,
            crate::bytes!("F3 0F 10 8B 00 0C 00 00 0F 57 C0 F3 0F 5C CE"),
            PressureKind::Consumption1RbxXmm1,
        ),
        pressure(
            0x1600,
            crate::bytes!("F3 0F 10 83 00 0C 00 00 F3 0F 5C C7"),
            PressureKind::Consumption2RbxXmm0,
        ),
    ];
    for site in cases {
        let cave = built(&site).unwrap();
        assert_eq!(cave.exits(), [Exit::Jump(cont(&site))], "{:?}", site.kind);
        let reentrant = site.kind == HookKind::NoReload(ReloadKind::DirectCallbackRcx);
        assert_eq!(cave.returns_into_cave(), reentrant);
        assert_eq!(matches!(cave, Built::ReturnsInto(_)), reentrant);
    }
}

#[test]
fn originals_that_are_not_the_reviewed_shape_are_refused() {
    let callee = Rva::new(0x2000);
    let cases = [
        // A relative branch cannot be copied.
        gate(
            0x1000,
            crate::bytes!("75 1A 90 90 90"),
            GateEffect::ReturnTrue,
        ),
        // Nor can a RIP-relative load.
        gate(
            0x1000,
            crate::bytes!("48 8B 05 00 00 00 00"),
            GateEffect::ReturnTrue,
        ),
        // An instruction cut short.
        gate(
            0x1000,
            crate::bytes!("90 90 48 8B 05 00"),
            GateEffect::ReturnTrue,
        ),
        // A load without the subtraction after it.
        gate(
            0x1000,
            crate::bytes!("49 8B 06 90 90"),
            GateEffect::ReserveLoad,
        ),
        // A multiply into xmm1, not xmm9.
        gate(
            0x1000,
            crate::bytes!("F3 0F 59 CA 90"),
            GateEffect::ZeroXmm9,
        ),
        // A call to another callee.
        gate(
            0x1000,
            crate::bytes!("E8 00 00 00 00"),
            GateEffect::SkipBatteryCall { callee },
        ),
        // An `or`, not an `and`.
        gate(
            0x1000,
            crate::bytes!("41 09 80 78 04 00 00"),
            GateEffect::SkipMedkitAnd,
        ),
        // A RIP-relative indirect call.
        gate(
            0x1000,
            crate::bytes!("FF 15 00 00 00 00"),
            GateEffect::SkipMaskCall,
        ),
        // A call through a register, not memory.
        gate(0x1000, crate::bytes!("FF D0"), GateEffect::SkipMaskCall),
        // A call through an absolute address.
        gate(
            0x1000,
            crate::bytes!("FF 14 25 00 10 00 00"),
            GateEffect::SkipMaskCall,
        ),
        // A direct callback without its call.
        site(
            0x1000,
            crate::bytes!("8D 50 FF 48 8B 46 F8"),
            HookKind::NoReload(ReloadKind::DirectCallbackRcx),
        ),
        // An event callback with a call: the game would return into a cave typed otherwise.
        site(
            0x1000,
            crate::bytes!("8D 50 FF FF 93 60 13 00 00"),
            HookKind::NoReload(ReloadKind::EventCallbackRcx),
        ),
        // No `lea edx,[rax-1]` to replace.
        site(
            0x1000,
            crate::bytes!("48 8B CF 8D 50 FE"),
            HookKind::NoReload(ReloadKind::HellsingFireRdi),
        ),
        // A consumption whose result is in xmm0, declared as xmm1.
        site(
            0x1000,
            crate::bytes!("F3 0F 10 83 00 0C 00 00 F3 0F 5C C7"),
            HookKind::Pressure {
                bit: ControlBit::new(31),
                kind: PressureKind::Consumption1RbxXmm1,
            },
        ),
    ];
    for site in cases {
        assert!(
            matches!(built(&site), Err(HookError::Original { .. })),
            "{:?} {:02X?}: {:?}",
            site.kind,
            site.original,
            built(&site)
        );
    }
}

#[test]
fn layouts_need_an_aligned_cave_and_control_word() {
    assert_eq!(
        Layout::new(IMAGE, Va::new(CAVE.get() + 8), CONTROLS),
        Err(HookError::UnalignedCave(Va::new(CAVE.get() + 8)))
    );
    assert_eq!(
        Layout::new(IMAGE, CAVE, Va::new(CONTROLS.get() + 2)),
        Err(HookError::UnalignedControls(Va::new(CONTROLS.get() + 2)))
    );
    let valid = ValidProfile::new(&SYNTHETIC).unwrap();
    assert!(matches!(
        plan(&valid, IMAGE, Va::new(CAVE.get() + 0x10), CONTROLS),
        Err(HookError::UnalignedCave(_))
    ));
}

#[test]
fn site_jumps_are_e9_rel32_padded_with_nops() {
    let at = Va::new(0x1000);
    assert_eq!(
        site_jump(SiteId(1), at, Va::new(0x2000), 7).unwrap(),
        [0xE9, 0xFB, 0x0F, 0x00, 0x00, 0x90, 0x90]
    );
    assert_eq!(
        site_jump(SiteId(1), at, Va::new(0x2000), 4),
        Err(HookError::ShortSite {
            site: SiteId(1),
            len: 4
        })
    );
    let far = Va::new(0x1005 + i32::MAX as u64 + 1);
    assert!(matches!(
        site_jump(SiteId(1), at, far, 5),
        Err(HookError::Address { .. })
    ));
    assert!(site_jump(SiteId(1), at, Va::new(0x1005 + i32::MAX as u64), 5).is_ok());
}

#[test]
fn every_error_is_a_bug_report() {
    assert_eq!(
        HookError::UnalignedCave(CAVE).recovery(),
        crate::recovery::RecoveryAction::ReportBug
    );
}

// The proof, tried on hand-assembled caves. `minimal` is the smallest valid cave; each test breaks one rule.

fn finish(exits: Vec<Exit>, emit: impl FnOnce(&mut Emitter) -> Result<(), Fault>) -> Draft {
    let mut e = Emitter::new().unwrap();
    emit(&mut e).unwrap();
    e.finish(CAVE, exits).unwrap()
}

fn prove<R: Reentry>(draft: Draft) -> Result<Cave<R>, HookError> {
    draft.prove(SiteId(1), layout())
}

fn refusal<R: Reentry>(draft: Draft) -> ProofError {
    match prove::<R>(draft) {
        Err(HookError::Proof { reason, .. }) => reason,
        other => panic!("expected a proof failure, got {other:?}"),
    }
}

const NEXT: Va = Va::new(0x1_4000_1005);
const ELSEWHERE: Va = Va::new(0x1_4000_2000);

/// `pushfq; push rax; mov rax, controls; mov eax,[rax]`.
fn snapshot(e: &mut Emitter) -> Result<(), Fault> {
    e.asm().pushfq()?;
    e.asm().push(rax)?;
    e.asm().mov(rax, CONTROLS.get())?;
    e.asm().mov(eax, dword_ptr(rax))?;
    Ok(())
}

fn unwind(e: &mut Emitter) -> Result<(), Fault> {
    e.asm().pop(rax)?;
    Ok(e.asm().popfq()?)
}

fn minimal() -> Draft {
    finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        unwind(e)?;
        e.jump(NEXT)
    })
}

#[test]
fn the_minimal_cave_passes() {
    let cave = prove::<NoCaveReturn>(minimal()).unwrap();
    assert_eq!(cave.exits(), [Exit::Jump(NEXT)]);
    assert_eq!(cave.literals().len(), 1);
    assert_eq!(cave.literals()[0].value(), NEXT.get());
    assert_eq!(
        cave.code().end(),
        CAVE.get() + u64::from(cave.literals()[0].offset())
    );
}

#[test]
fn proof_requires_exactly_one_control_load_on_every_path() {
    let none = finish(vec![Exit::Jump(NEXT)], |e| e.jump(NEXT));
    assert_eq!(refusal::<NoCaveReturn>(none), ProofError::ControlLoads(0));
    let twice = finish(vec![Exit::Jump(NEXT)], |e| {
        e.asm().push(rcx)?;
        e.asm().push(rax)?;
        e.asm().mov(rcx, CONTROLS.get())?;
        e.asm().mov(eax, dword_ptr(rcx))?;
        e.asm().mov(eax, dword_ptr(rcx))?;
        e.asm().pop(rax)?;
        e.asm().pop(rcx)?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(twice), ProofError::ControlLoads(2));
    let reloaded = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        e.asm().mov(rax, CONTROLS.get())?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(
        refusal::<NoCaveReturn>(reloaded),
        ProofError::ControlAddress
    );
    let pooled = finish(vec![Exit::Jump(NEXT), Exit::Jump(CONTROLS)], |e| {
        let mut other = e.label();
        snapshot(e)?;
        e.asm().test(eax, 1)?;
        e.asm().jnz(other)?;
        unwind(e)?;
        e.jump(NEXT)?;
        e.bind(&mut other)?;
        unwind(e)?;
        e.jump(CONTROLS)
    });
    assert_eq!(refusal::<NoCaveReturn>(pooled), ProofError::ControlAddress);
}

#[test]
fn proof_refuses_any_other_use_of_the_control_word() {
    let store = finish(vec![Exit::Jump(NEXT)], |e| {
        e.asm().pushfq()?;
        e.asm().push(rax)?;
        e.asm().mov(rax, CONTROLS.get())?;
        e.asm().mov(dword_ptr(rax), 0)?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(store), ProofError::ControlAccess);
    let copied = finish(vec![Exit::Jump(NEXT)], |e| {
        e.asm().pushfq()?;
        e.asm().push(rax)?;
        e.asm().push(rdx)?;
        e.asm().mov(rax, CONTROLS.get())?;
        e.asm().mov(rdx, rax)?;
        e.asm().mov(eax, dword_ptr(rax))?;
        e.asm().pop(rdx)?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(copied), ProofError::ControlAccess);
    // After a join where one path overwrote it, the register may or may not hold the address.
    let uncertain = finish(vec![Exit::Jump(NEXT)], |e| {
        let mut join = e.label();
        e.asm().pushfq()?;
        e.asm().push(rax)?;
        e.asm().push(rdx)?;
        e.asm().mov(rax, CONTROLS.get())?;
        e.asm().mov(edx, dword_ptr(rax))?;
        e.asm().test(edx, 1)?;
        e.asm().jz(join)?;
        e.asm().mov(rax, 0_u64)?;
        e.bind(&mut join)?;
        e.asm().mov(edx, dword_ptr(rax))?;
        e.asm().pop(rdx)?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(
        refusal::<NoCaveReturn>(uncertain),
        ProofError::ControlAccess
    );
}

#[test]
fn proof_requires_saved_registers_and_flags() {
    let register = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        e.asm().mov(rcx, 1_u64)?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(
        refusal::<NoCaveReturn>(register),
        ProofError::UnsavedRegister(Register::RCX)
    );
    let flags = finish(vec![Exit::Jump(NEXT)], |e| {
        e.asm().push(rax)?;
        e.asm().mov(rax, CONTROLS.get())?;
        e.asm().mov(eax, dword_ptr(rax))?;
        e.asm().test(eax, 1)?;
        e.asm().pop(rax)?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(flags), ProofError::UnsavedFlags);
    let swapped = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        e.asm().popfq()?;
        e.asm().pop(rax)?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(swapped), ProofError::WrongRestore);
    let wrong_register = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        e.asm().pop(rcx)?;
        e.asm().popfq()?;
        e.jump(NEXT)
    });
    assert_eq!(
        refusal::<NoCaveReturn>(wrong_register),
        ProofError::WrongRestore
    );
}

#[test]
fn proof_bounds_stack_and_memory() {
    let unbalanced = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        e.asm().pop(rax)?;
        e.jump(NEXT)
    });
    assert_eq!(
        refusal::<NoCaveReturn>(unbalanced),
        ProofError::UnbalancedExit
    );
    let game_memory = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        e.asm().mov(qword_ptr(rbx + 8), rax)?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(
        refusal::<NoCaveReturn>(game_memory),
        ProofError::MemoryWrite
    );
    let past_frame = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        e.asm().sub(rsp, 0x10)?;
        e.asm().mov(qword_ptr(rsp + 0x10), rax)?;
        e.asm().add(rsp, 0x10)?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(
        refusal::<NoCaveReturn>(past_frame),
        ProofError::OutsideFrame
    );
    let odd = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        e.asm().sub(rsp, 4)?;
        e.asm().add(rsp, 4)?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(odd), ProofError::UnsupportedStack);
    let flags_after_unwind = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        unwind(e)?;
        e.asm().sub(rsp, 8)?;
        e.asm().add(rsp, 8)?;
        e.jump(NEXT)
    });
    assert_eq!(
        refusal::<NoCaveReturn>(flags_after_unwind),
        ProofError::UnsavedFlags
    );
    let indexed = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        e.asm().sub(rsp, 0x10)?;
        e.asm().mov(qword_ptr(rsp + rbx), rax)?;
        e.asm().add(rsp, 0x10)?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(indexed), ProofError::OutsideFrame);
    let moved = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        e.asm().mov(rsp, rax)?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(moved), ProofError::OutsideFrame);
    // A game push while the cave's flags and RAX are still on the stack.
    let hidden = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        e.game(|asm| asm.push(r8))?;
        e.game(|asm| asm.pop(r8))?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(hidden), ProofError::GameStackHidden);
}

#[test]
fn proof_holds_calls_to_the_cave_type() {
    let call = || {
        finish(vec![Exit::Jump(NEXT)], |e| {
            snapshot(e)?;
            unwind(e)?;
            e.game(|asm| asm.call(qword_ptr(rbx + 0x1360)))?;
            e.jump(NEXT)
        })
    };
    assert_eq!(refusal::<NoCaveReturn>(call()), ProofError::UnexpectedCall);
    assert!(prove::<ReturnsIntoCave>(call()).is_ok());
    assert_eq!(
        refusal::<ReturnsIntoCave>(minimal()),
        ProofError::MissingCall
    );
    let own_call = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        unwind(e)?;
        e.asm().call(qword_ptr(rbx + 0x1360))?;
        e.jump(NEXT)
    });
    assert_eq!(
        refusal::<ReturnsIntoCave>(own_call),
        ProofError::UnexpectedCall
    );
}

#[test]
fn proof_checks_exits_and_flow() {
    let undeclared = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        unwind(e)?;
        e.jump(ELSEWHERE)
    });
    assert_eq!(
        refusal::<NoCaveReturn>(undeclared),
        ProofError::UndeclaredExit(Exit::Jump(ELSEWHERE))
    );
    let missing = finish(vec![Exit::Jump(NEXT), Exit::Return], |e| {
        snapshot(e)?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(
        refusal::<NoCaveReturn>(missing),
        ProofError::MissingExit(Exit::Return)
    );
    let backwards = finish(vec![Exit::Jump(NEXT)], |e| {
        let mut top = e.label();
        snapshot(e)?;
        e.bind(&mut top)?;
        e.asm().test(eax, 1)?;
        e.asm().jnz(top)?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(
        refusal::<NoCaveReturn>(backwards),
        ProofError::BackwardBranch
    );
    let falls = finish(vec![Exit::Jump(NEXT)], |e| {
        let mut end = e.label();
        snapshot(e)?;
        unwind(e)?;
        e.asm().jmp(end)?;
        e.bind(&mut end)?;
        e.asm().nop()?;
        Ok(())
    });
    assert_eq!(
        refusal::<NoCaveReturn>(falls),
        ProofError::FallsIntoLiterals
    );
    let dead = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        unwind(e)?;
        e.jump(NEXT)?;
        e.asm().nop()?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(dead), ProofError::Unreachable);
    let disagree = finish(vec![Exit::Jump(NEXT)], |e| {
        let mut join = e.label();
        snapshot(e)?;
        e.asm().test(eax, 1)?;
        e.asm().jz(join)?;
        e.asm().push(rax)?;
        e.bind(&mut join)?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(disagree), ProofError::PathsDisagree);
    let register_jump = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        unwind(e)?;
        e.asm().jmp(rax)?;
        e.jump(NEXT)
    });
    assert_eq!(
        refusal::<NoCaveReturn>(register_jump),
        ProofError::UnsupportedFlow
    );
    let trap = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        unwind(e)?;
        e.asm().int3()?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(trap), ProofError::UnsupportedFlow);
}

#[test]
fn proof_keeps_code_and_literals_apart() {
    let stray = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        let literal = e.literal(NEXT.get());
        e.asm().mov(rax, qword_ptr(literal))?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(stray), ProofError::StrayRipRelative);
    let unused = finish(vec![Exit::Jump(NEXT)], |e| {
        let _ = e.literal(ELSEWHERE.get());
        snapshot(e)?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(unused), ProofError::UnusedLiteral);
    let mut tampered = minimal();
    let last = tampered.bytes.len() - 1;
    tampered.bytes[last] ^= 0xFF;
    assert_eq!(refusal::<NoCaveReturn>(tampered), ProofError::LiteralPool);
    let mut invalid = minimal();
    invalid.bytes[0] = 0x06;
    assert_eq!(refusal::<NoCaveReturn>(invalid), ProofError::Undecodable);
}

#[test]
fn proof_failures_name_the_site_and_offset() {
    let draft = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        e.asm().mov(rcx, 1_u64)?;
        unwind(e)?;
        e.jump(NEXT)
    });
    let error = prove::<NoCaveReturn>(draft).unwrap_err();
    let HookError::Proof { site, offset, .. } = error else {
        panic!("{error:?}");
    };
    assert_eq!(site, SiteId(1));
    // pushfq (1) + push rax (1) + movabs (10) + mov eax,[rax] (2).
    assert_eq!(offset, 14);
    assert!(
        error
            .to_string()
            .starts_with("S1: the cave failed its proof at offset 0xe")
    );
}

#[test]
fn caves_larger_than_a_page_are_refused() {
    let draft = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        for _ in 0..PAGE {
            e.asm().nop()?;
        }
        unwind(e)?;
        e.jump(NEXT)
    });
    assert!(matches!(
        prove::<NoCaveReturn>(draft),
        Err(HookError::TooLarge { .. })
    ));
}

// Placement.

#[test]
fn reach_is_exact_at_both_rel32_endpoints() {
    let reach = Reach::new(&SYNTHETIC, IMAGE).unwrap();
    assert_eq!(reach.size(), allocation_len(&SYNTHETIC));
    let page = u64::from(PAGE);
    let jump = |base: u64, index: usize| {
        let site = IMAGE.at(SYNTHETIC.sites[index].rva).unwrap();
        crate::addr::Rel32::from_site(site, Va::new(base + index as u64 * page))
    };
    for index in 0..SYNTHETIC.sites.len() {
        assert!(jump(reach.lowest().get(), index).is_ok());
        assert!(jump(reach.highest().get(), index).is_ok());
    }
    // One byte further, some site no longer reaches its cave.
    let below = reach.lowest().get() - 1;
    let above = reach.highest().get() + 1;
    assert!((0..SYNTHETIC.sites.len()).any(|index| jump(below, index).is_err()));
    assert!((0..SYNTHETIC.sites.len()).any(|index| jump(above, index).is_err()));
    assert!(reach.contains(reach.middle()));
    assert!(!reach.contains(Va::new(below)) && !reach.contains(Va::new(above)));
}

#[test]
fn reach_is_none_without_sites_or_past_the_address_space() {
    let near_top = Va::new(u64::MAX - 0x1000);
    assert_eq!(Reach::new(&SYNTHETIC, near_top), None);
    // The last site starts in the address space but its jump would end past it.
    let last = SYNTHETIC.sites[3].rva.get();
    assert_eq!(
        Reach::new(&SYNTHETIC, Va::new(u64::MAX - u64::from(last) - 3)),
        None
    );
    let none = Profile {
        sites: &[],
        ..SYNTHETIC
    };
    assert_eq!(Reach::new(&none, IMAGE), None);
}

#[test]
fn no_candidate_is_address_zero() {
    let reach = Reach::new(&SYNTHETIC, Va::new(0x1000)).unwrap();
    assert_eq!(reach.lowest(), Va::new(u64::from(PAGE)));
    // Only bases 0 and 0x10000 fit this region; 0 is never offered.
    let free = [FreeRegion {
        start: Va::new(0),
        len: 0x2_0000,
    }];
    assert_eq!(candidates(reach, 0x1_0000, free), [Va::new(0x1_0000)]);
}

#[test]
fn candidates_are_aligned_inside_and_nearest_first() {
    let reach = Reach::new(&SYNTHETIC, IMAGE).unwrap();
    let granularity = 0x1_0000;
    let middle = reach.middle().get();
    let free = [
        FreeRegion {
            start: Va::new(middle + 0x10_0123),
            len: 0x10_0000,
        },
        FreeRegion {
            start: Va::new(middle - 0x5_0000),
            len: 0x2_0000,
        },
        // Too small for the allocation once aligned.
        FreeRegion {
            start: Va::new(middle + 0x1234),
            len: 0x4000,
        },
        // Outside the reach.
        FreeRegion {
            start: Va::new(reach.highest().get() + 0x1_0000),
            len: 0x100_0000,
        },
    ];
    let found = candidates(reach, granularity, free);
    // Each region offers its aligned base nearest the middle: the highest that fits below it, the lowest
    // above it.
    let below = (middle - 0x3_0000 - reach.size()) & !(granularity - 1);
    let above = (middle + 0x10_0123).next_multiple_of(granularity);
    assert_eq!(found, [Va::new(below), Va::new(above)]);
    assert!(candidates(reach, 0x3000, free).is_empty());
    assert!(candidates(reach, 0x800, free).is_empty());
    let edge = [FreeRegion {
        start: Va::new(u64::MAX - 0x1000),
        len: 0x1000,
    }];
    assert!(candidates(reach, granularity, edge).is_empty());
}

proptest! {
    #[test]
    fn every_candidate_fits_its_region_and_the_reach(
        regions in proptest::collection::vec((0u64..0x4_0000_0000, 0u64..0x1000_0000), 0..8),
        shift in 0u64..0x2_0000_0000,
    ) {
        // Images from the bottom of the address space up, so the reach is clamped at its low end too.
        let image = Va::new(shift & !0xFFF);
        let reach = Reach::new(&SYNTHETIC, image).unwrap();
        let granularity = 0x1_0000;
        let free: Vec<FreeRegion> = regions
            .iter()
            .map(|&(start, len)| FreeRegion { start: Va::new(start), len })
            .collect();
        let found = candidates(reach, granularity, free.iter().copied());
        let middle = reach.middle().get();
        for pair in found.windows(2) {
            prop_assert!(pair[0].get().abs_diff(middle) <= pair[1].get().abs_diff(middle));
        }
        for base in found {
            prop_assert!(base.get() != 0);
            prop_assert_eq!(base.get() % granularity, 0);
            prop_assert!(reach.contains(base));
            let end = base.get() + reach.size();
            prop_assert!(free.iter().any(|region| region.start.get() <= base.get()
                && end <= region.start.get() + region.len));
            prop_assert!(plan(&ValidProfile::new(&SYNTHETIC).unwrap(), image, base, CONTROLS).is_ok());
        }
    }
}

#[test]
fn proof_keeps_game_instructions_on_the_game_state() {
    // Replaying the original before restoring: the game would see the cave's RAX and flags.
    let early = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        e.game(|asm| asm.mov(rcx, qword_ptr(rbx)))?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(early), ProofError::GameStackHidden);
    // A game jump through a register, even with a literal return address pushed.
    let through_register = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        unwind(e)?;
        let ret = e.literal(NEXT.get());
        e.asm().push(qword_ptr(ret))?;
        e.game(|asm| asm.jmp(rax))
    });
    assert_eq!(
        refusal::<NoCaveReturn>(through_register),
        ProofError::UnsupportedFlow
    );
}

#[test]
fn proof_allows_one_declared_effect_per_path() {
    let store = |times: usize| {
        finish(vec![Exit::Jump(NEXT)], move |e| {
            snapshot(e)?;
            for _ in 0..times {
                e.effect(|asm| asm.movss(dword_ptr(rbx + 0xC00), xmm2))?;
            }
            unwind(e)?;
            e.jump(NEXT)
        })
    };
    assert!(prove::<NoCaveReturn>(store(1)).is_ok());
    assert_eq!(
        refusal::<NoCaveReturn>(store(2)),
        ProofError::TooManyEffects
    );
}

#[test]
fn proof_refuses_other_ways_to_reach_the_control_word() {
    let offset = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        e.asm().push(rcx)?;
        e.asm().mov(rcx, CONTROLS.get() - 0x10)?;
        e.asm().mov(eax, dword_ptr(rcx + 0x10))?;
        e.asm().pop(rcx)?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(offset), ProofError::ControlAddress);
    let displaced = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        e.asm().push(rcx)?;
        e.asm().mov(rcx, CONTROLS.get() - 0x2_0000)?;
        e.asm().mov(eax, dword_ptr(rcx + 0x2_0000))?;
        e.asm().pop(rcx)?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(
        refusal::<NoCaveReturn>(displaced),
        ProofError::FarDisplacement
    );
    let absolute = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        // mov eax, [0x1000]
        e.asm().db(&[0x8B, 0x04, 0x25, 0x00, 0x10, 0x00, 0x00])?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(
        refusal::<NoCaveReturn>(absolute),
        ProofError::AbsoluteAddress
    );
    let segment = finish(vec![Exit::Jump(NEXT)], |e| {
        e.asm().pushfq()?;
        e.asm().push(rax)?;
        e.asm().mov(rax, CONTROLS.get())?;
        e.asm().mov(eax, dword_ptr(rax).gs())?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(segment), ProofError::ControlAccess);
}

type Emit = fn(&mut Emitter) -> Result<(), Fault>;

/// A minimal cave with RDX saved and a 16-byte frame reserved around `emit`.
fn with_frame(emit: Emit) -> Draft {
    finish(vec![Exit::Jump(NEXT)], move |e| {
        snapshot(e)?;
        e.asm().push(rdx)?;
        e.asm().sub(rsp, 0x10)?;
        emit(e)?;
        e.asm().add(rsp, 0x10)?;
        e.asm().pop(rdx)?;
        unwind(e)?;
        e.jump(NEXT)
    })
}

#[test]
fn proof_refuses_stack_aliases_and_unsized_frame_operands() {
    let copy: Emit = |e| Ok(e.asm().mov(rdx, rsp)?);
    let address: Emit = |e| Ok(e.asm().lea(rdx, ptr(rsp + 0x100))?);
    let narrow: Emit = |e| Ok(e.asm().mov(edx, dword_ptr(esp + 8))?);
    let unsized_save: Emit = |e| Ok(e.asm().xsave(ptr(rsp))?);
    let push_rsp: Emit = |e| {
        e.asm().push(rsp)?;
        Ok(e.asm().add(rsp, 8)?)
    };
    for emit in [copy, address, narrow, unsized_save, push_rsp] {
        assert_eq!(
            refusal::<NoCaveReturn>(with_frame(emit)),
            ProofError::OutsideFrame
        );
    }
    let fits: Emit = |e| Ok(e.asm().mov(qword_ptr(rsp + 8), rdx)?);
    assert!(prove::<NoCaveReturn>(with_frame(fits)).is_ok());
}

#[test]
fn proof_refuses_branches_out_of_the_code() {
    let out = finish(vec![Exit::Jump(NEXT)], |e| {
        snapshot(e)?;
        e.asm().test(eax, 1)?;
        e.asm().jnz(ELSEWHERE.get())?;
        unwind(e)?;
        e.jump(NEXT)
    });
    assert_eq!(refusal::<NoCaveReturn>(out), ProofError::BranchTarget);
}
