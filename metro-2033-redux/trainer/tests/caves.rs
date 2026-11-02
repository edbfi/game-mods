// SPDX-License-Identifier: AGPL-3.0-only
//! The Metro hook caves: how they are planned and placed, and, on Linux x86-64, what they do when they run.
//!
//! The execution tests lay out a stand-in for the game in one mapping: the image at its base, with each site's
//! original bytes or its hook jump at the real RVA and a return after it, mock callees at the reviewed call
//! targets, the class-id and manager globals, a weapon, its configuration and the player. Each case runs the
//! site twice from the same seed, once as the game's own code and once hooked, and compares every register,
//! the flags, all XMM lanes, MXCSR and the memory the site can reach.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use edbfi_trainer::addr::Va;
use edbfi_trainer::cave::place::{FreeRegion, Reach, candidates};
use edbfi_trainer::cave::{self, Callee, Exit, HookPlan, PAGE};
use edbfi_trainer::profile::{GateEffect, HookKind, ValidProfile};
use metro_2033_redux_trainer::profile::METRO;

const IMAGE: u64 = 0x1_4000_0000;
const CONTROLS: u64 = 0x1_2000_0000;

fn valid() -> ValidProfile<'static> {
    ValidProfile::new(&METRO).unwrap()
}

fn metro_plan(allocation: u64) -> Result<HookPlan, cave::HookError> {
    cave::plan(
        &valid(),
        Va::new(IMAGE),
        Va::new(allocation),
        Va::new(CONTROLS),
    )
}

#[test]
fn every_site_gets_a_proven_cave_on_its_own_page() {
    let reach = Reach::new(&METRO, Va::new(IMAGE)).unwrap();
    let plan = metro_plan(reach.middle().get() & !0xFFFF).unwrap();
    assert_eq!(plan.hooks().len(), 17);
    for (page, hook) in plan.hooks().iter().enumerate() {
        let cave = hook.cave();
        assert_eq!(
            cave.address().get(),
            plan.allocation().get() + page as u64 * u64::from(PAGE)
        );
        assert!(!cave.bytes().is_empty() && cave.bytes().len() <= PAGE as usize);
        // The site jump is `E9 rel32` from the end of the jump, padded with NOPs to the original length.
        let jump = hook.jump();
        assert_eq!(jump.len(), hook.original().len());
        assert_eq!(jump[0], 0xE9);
        let rel = i32::from_le_bytes(jump[1..5].try_into().unwrap());
        let next = i128::from(hook.at().get()) + 5;
        assert_eq!(next + i128::from(rel), i128::from(cave.address().get()));
        assert!(jump[5..].iter().all(|byte| *byte == 0x90));
        assert_eq!(hook.at(), Va::new(IMAGE + u64::from(hook.site().rva.get())));
    }
}

#[test]
fn only_the_direct_callback_cave_is_returned_into() {
    let plan = metro_plan(0x1_3000_0000).unwrap();
    let returned: Vec<_> = plan
        .hooks()
        .iter()
        .filter(|hook| hook.cave().returns_into_cave())
        .map(|hook| hook.site().id.0)
        .collect();
    assert_eq!(returned, [12]);
    assert!(plan.returns_into_cave());
}

#[test]
fn each_cave_leaves_only_where_its_site_allows() {
    let plan = metro_plan(0x1_3000_0000).unwrap();
    let at = |rva: u32| Va::new(IMAGE + u64::from(rva));
    for hook in plan.hooks() {
        let cont = Va::new(hook.at().get() + hook.original().len() as u64);
        let mut want = match hook.site().kind {
            HookKind::Gate { effect, .. } => match effect {
                GateEffect::ReturnTrue => vec![Exit::Jump(cont), Exit::Return],
                GateEffect::Throwable { callee, enabled } => vec![
                    Exit::TailCall {
                        callee: Callee::Fixed(at(callee.get())),
                        ret: cont,
                    },
                    Exit::TailCall {
                        callee: Callee::Fixed(at(enabled.get())),
                        ret: cont,
                    },
                ],
                GateEffect::SkipMaskCall => vec![
                    Exit::TailCall {
                        callee: Callee::Indirect,
                        ret: cont,
                    },
                    Exit::Jump(cont),
                ],
                GateEffect::GodBranch { target } => {
                    vec![Exit::Jump(cont), Exit::Jump(at(target.get()))]
                }
                GateEffect::SkipBatteryCall { callee } => vec![
                    Exit::TailCall {
                        callee: Callee::Fixed(at(callee.get())),
                        ret: cont,
                    },
                    Exit::Jump(cont),
                ],
                GateEffect::ReserveLoad | GateEffect::ZeroXmm9 | GateEffect::SkipMedkitAnd => {
                    vec![Exit::Jump(cont)]
                }
            },
            HookKind::NoReload(_) | HookKind::Pressure { .. } => vec![Exit::Jump(cont)],
        };
        want.sort_unstable();
        assert_eq!(hook.cave().exits(), want, "{}", hook.site().id);
    }
}

#[test]
fn the_allocation_reaches_every_site_from_both_ends_and_no_further() {
    let reach = Reach::new(&METRO, Va::new(IMAGE)).unwrap();
    let page = u64::from(PAGE);
    let first = reach.lowest().get().div_ceil(page) * page;
    let last = reach.highest().get() / page * page;
    assert!(metro_plan(first).is_ok());
    assert!(metro_plan(last).is_ok());
    // Page-aligned bases one page outside each end fail with an out-of-range jump.
    for base in [first - page, last + page] {
        assert!(matches!(
            metro_plan(base),
            Err(cave::HookError::Address { .. })
        ));
    }
}

#[test]
fn placement_prefers_free_memory_near_the_sites() {
    let reach = Reach::new(&METRO, Va::new(IMAGE)).unwrap();
    let granularity = 0x1_0000;
    let image_end = IMAGE + 0x200_0000;
    let free = [
        FreeRegion {
            start: Va::new(0x1000),
            len: 0x1_0000_0000,
        },
        FreeRegion {
            start: Va::new(image_end),
            len: 0x100_0000,
        },
        FreeRegion {
            start: Va::new(IMAGE - 0x40_0000),
            len: 0x8000,
        },
    ];
    let found = candidates(reach, granularity, free);
    assert_eq!(found[0], Va::new(image_end));
    for base in &found {
        assert_eq!(base.get() % granularity, 0);
        assert!(reach.contains(*base));
        assert!(metro_plan(base.get()).is_ok());
    }
    // The third region is smaller than the allocation.
    assert_eq!(found.len(), 2);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod exec;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod execution {
    use std::time::{Duration, Instant};

    use edbfi_trainer::addr::Rva;
    use edbfi_trainer::cave::Hook;
    use edbfi_trainer::profile::{PressureKind, ReloadKind, Site};
    use iced_x86::Register;
    use iced_x86::code_asm::{dword_ptr, eax, qword_ptr, rax, rcx, rdx, rsp};

    use super::exec::{
        ARITHMETIC_FLAGS, Arena, CANARY, Ending, Machine, PAGE, RSP, State, assemble, forked,
    };
    use super::*;

    const ARENA: usize = 0x140_0000;
    const CAVES: usize = 0x100_0000;
    const CONTROL_WORD: usize = 0x102_0000;
    const WEAPON: usize = 0x103_0000;
    const WEAPON_LEN: usize = 0x1400;
    const CONFIG: usize = 0x103_4000;
    const CONFIG_LEN: usize = 0x1400;
    const HANDLE: usize = 0x103_8000;
    const MANAGER: usize = 0x103_9000;
    const PLAYER: usize = 0x103_A000;
    const STRANGER: usize = 0x103_B000;
    const OBSERVED: usize = 0x103_C000;
    const OBSERVED_LEN: usize = 0x200;
    const RENDEZVOUS: usize = 0x103_D000;
    /// Another actor's weapon, in every register that is not the site's receiver: a cave that classifies the
    /// wrong register sees a stranger's weapon and leaves it alone.
    const DECOY: usize = 0x103_E000;
    const DECOY_HANDLE: usize = HANDLE + 0x100;
    /// A weapon whose owner field lies in the guard page after it.
    const EXPOSED: usize = 0x104_1000 - 0x100;
    const GUARD: usize = 0x104_1000;
    /// A weapon inside a second guard page whose pressure lies in the readable page after it, for the
    /// pressure sites, whose originals read the pressure.
    const GUARD2: usize = 0x104_3000;
    const EXPOSED2: usize = GUARD2 + 0x800;
    const STACK: usize = 0x110_0000;
    const STACK_LEN: usize = 0x2000;
    const ENTRY: usize = STACK + STACK_LEN - 0x408;
    const STACK2: usize = 0x112_0000;
    const ENTRY2: usize = STACK2 + STACK_LEN - 0x408;
    const MACHINE: usize = 0x120_0000;
    const MACHINE2: usize = 0x120_1000;
    const BLOCK: usize = 0x121_0000;
    const BLOCK2: usize = 0x121_1000;
    /// Mock code inside the image, away from every site.
    const MOCKS: usize = 0x60_0000;
    const IDENTITY: usize = MOCKS;
    const BLOCKER: usize = MOCKS + 0x800;
    /// How far below the entry RSP a run may write. Below RSP is scratch (Win64 has no red zone), so runs
    /// are not compared there, only checked to stay within this bound.
    const STACK_BELOW: usize = 0x200;
    /// The caller's side of the stack a case compares: from the slot above the return address (the shadow
    /// space a callee may use) up.
    const STACK_ABOVE: usize = 0x400;

    /// The observation slot of each mock callee.
    #[derive(Clone, Copy)]
    enum Mock {
        Callee = 0,
        Enabled = 1,
        Battery = 2,
        Virtual = 3,
        Setter = 4,
    }

    impl Mock {
        fn slot(self) -> usize {
            OBSERVED + self as usize * 0x40
        }

        fn code(self) -> usize {
            MOCKS + 0x100 * (1 + self as usize)
        }
    }

    /// What a run left behind.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Outcome {
        state: State,
        weapon: Vec<u8>,
        config: Vec<u8>,
        observed: Vec<u8>,
        stack: Vec<u8>,
    }

    impl Outcome {
        fn observation(&self, mock: Mock) -> [u64; 4] {
            let at = mock as usize * 0x40;
            std::array::from_fn(|i| {
                u64::from_le_bytes(
                    self.observed[at + 8 * i..at + 8 * i + 8]
                        .try_into()
                        .unwrap(),
                )
            })
        }

        fn set_observation(&mut self, mock: Mock, value: [u64; 4]) {
            let at = mock as usize * 0x40;
            for (i, word) in value.iter().enumerate() {
                self.observed[at + 8 * i..at + 8 * i + 8].copy_from_slice(&word.to_le_bytes());
            }
        }
    }

    /// The stand-in game.
    struct World {
        arena: Arena,
        machine: Machine,
        plan: HookPlan,
        ids: Vec<Vec<u32>>,
    }

    impl World {
        fn new() -> Self {
            let arena = Arena::new(ARENA);
            let base = arena.base();
            let plan = cave::plan(
                &valid(),
                Va::new(base),
                Va::new(arena.at(CAVES)),
                Va::new(arena.at(CONTROL_WORD)),
            )
            .unwrap();
            for (page, hook) in plan.hooks().iter().enumerate() {
                arena.code(CAVES + page * PAGE, hook.cave().bytes());
            }
            let machine = Machine::new(&arena, MACHINE, BLOCK, ENTRY);
            // Distinct interned ids for every class and alias.
            let ids: Vec<Vec<u32>> = METRO
                .classes
                .iter()
                .enumerate()
                .map(|(class, data)| {
                    (0..data.interned_ids.len())
                        .map(|alias| 0x5100_0000 + (class as u32) * 0x100 + alias as u32)
                        .collect()
                })
                .collect();
            for (class, data) in METRO.classes.iter().enumerate() {
                for (alias, rva) in data.interned_ids.iter().enumerate() {
                    arena.put_u32(rva.get() as usize, ids[class][alias]);
                }
            }
            // Mocks: a bare return, recorders behind the reviewed call targets, and the god-mode target.
            arena.code(IDENTITY, &[0xC3]);
            for mock in [
                Mock::Callee,
                Mock::Enabled,
                Mock::Battery,
                Mock::Virtual,
                Mock::Setter,
            ] {
                let slot = arena.at(mock.slot());
                arena.code(
                    mock.code(),
                    &assemble(arena.at(mock.code()), |a| recorder(a, slot)),
                );
            }
            arena.code(
                BLOCKER,
                &assemble(arena.at(BLOCKER), |a| {
                    let mut spin = a.create_label();
                    a.push(rax)?;
                    a.mov(rax, arena.at(RENDEZVOUS))?;
                    a.mov(dword_ptr(rax), 1)?;
                    a.set_label(&mut spin)?;
                    a.pause()?;
                    a.cmp(dword_ptr(rax + 4), 0)?;
                    a.je(spin)?;
                    a.pop(rax)?;
                    a.jmp(arena.at(Mock::Setter.code()))
                }),
            );
            let world = Self {
                arena,
                machine,
                plan,
                ids,
            };
            for hook in world.plan.hooks() {
                world.place_mocks(hook.site());
            }
            world.reset_objects();
            world
        }

        fn image(&self, rva: Rva) -> usize {
            rva.get() as usize
        }

        /// Recorders at the site's reviewed call targets, and the god-mode branch target.
        fn place_mocks(&self, site: &Site) {
            let jump_to = |at: usize, mock: Mock| {
                let target = self.arena.at(mock.code());
                self.arena
                    .code(at, &assemble(self.arena.at(at), |a| a.jmp(target)));
            };
            if let HookKind::Gate { effect, .. } = site.kind {
                match effect {
                    GateEffect::Throwable { callee, enabled } => {
                        jump_to(self.image(callee), Mock::Callee);
                        jump_to(self.image(enabled), Mock::Enabled);
                    }
                    GateEffect::SkipBatteryCall { callee } => {
                        jump_to(self.image(callee), Mock::Battery);
                    }
                    GateEffect::GodBranch { target } => {
                        let at = self.image(target);
                        self.arena.code(
                            at,
                            &assemble(self.arena.at(at), |a| {
                                a.mov(eax, 1)?;
                                a.ret()
                            }),
                        );
                    }
                    _ => {}
                }
            }
        }

        /// The player, the weapon, its configuration and the stack, as each case starts.
        fn reset_objects(&self) {
            let a = &self.arena;
            a.write(WEAPON, &[0x11; WEAPON_LEN]);
            a.write(CONFIG, &[0x22; CONFIG_LEN]);
            a.write(OBSERVED, &[0; OBSERVED_LEN]);
            let layout = METRO.player;
            a.put_u64(WEAPON + layout.weapon_owner as usize, a.at(HANDLE));
            a.put_u64(HANDLE, a.at(PLAYER) + u64::from(layout.owner));
            a.put_u64(WEAPON + layout.weapon_config as usize, a.at(CONFIG));
            a.put_u64(layout.manager.get() as usize, a.at(MANAGER));
            a.put_u64(MANAGER + layout.player as usize, a.at(PLAYER));
            a.put_u32(CONFIG, self.ids[0][0]);
            // The virtual mask call and the reload setter dispatch through the weapon.
            a.put_u64(WEAPON + 0x1B0, a.at(Mock::Virtual.code()));
            a.put_u64(WEAPON + 0x1360, a.at(Mock::Setter.code()));
            a.write(DECOY, &[0x33; WEAPON_LEN]);
            a.put_u64(DECOY + layout.weapon_owner as usize, a.at(DECOY_HANDLE));
            a.put_u64(DECOY_HANDLE, a.at(STRANGER) + u64::from(layout.owner));
            a.put_u64(DECOY + layout.weapon_config as usize, a.at(CONFIG));
            a.put_u64(DECOY + 0x1B0, a.at(Mock::Virtual.code()));
            a.put_u64(DECOY + 0x1360, a.at(Mock::Setter.code()));
            a.write(STACK, &[CANARY; STACK_LEN]);
            a.put_u32(RENDEZVOUS, 0);
            a.put_u32(RENDEZVOUS + 4, 1);
        }

        fn hook(&self, id: u8) -> &Hook {
            self.plan
                .hooks()
                .iter()
                .find(|hook| hook.site().id.0 == id)
                .unwrap()
        }

        fn cont(&self, hook: &Hook) -> usize {
            self.image(hook.site().rva) + hook.original().len()
        }

        /// Writes the site's original bytes, or its hook, followed by the code after the site.
        fn install(&self, hook: &Hook, hooked: bool) {
            let mut bytes = if hooked {
                hook.jump().to_vec()
            } else {
                hook.original().to_vec()
            };
            if let HookKind::Pressure {
                kind: PressureKind::RecoveryRcx,
                ..
            } = hook.site().kind
            {
                // The recovery original is a prologue: undo `push rbx; sub rsp,0x20` before returning.
                bytes.extend_from_slice(&[0x48, 0x83, 0xC4, 0x20, 0x5B]);
            }
            bytes.push(0xC3);
            self.arena.code(self.image(hook.site().rva), &bytes);
        }

        fn outcome(&self, state: State) -> Outcome {
            Outcome {
                state,
                weapon: self.arena.read(WEAPON, WEAPON_LEN),
                config: self.arena.read(CONFIG, CONFIG_LEN),
                observed: self.arena.read(OBSERVED, OBSERVED_LEN),
                stack: self.arena.read(ENTRY + 8, STACK_ABOVE - 8),
            }
        }

        /// Runs `hook`'s site from `seed` with the control word `controls`, after `setup`.
        fn run(
            &self,
            hook: &Hook,
            hooked: bool,
            controls: u32,
            seed: &State,
            setup: &dyn Fn(&Arena),
        ) -> Outcome {
            self.reset_objects();
            setup(&self.arena);
            self.arena.put_u32(CONTROL_WORD, controls);
            self.install(hook, hooked);
            let target = self.arena.at(self.image(hook.site().rva));
            let state = self.machine.run(&self.arena, target, seed);
            let outcome = self.outcome(state);
            self.assert_deep_stack_untouched();
            outcome
        }

        /// A run of a bare `ret`: everything as the seed left it.
        fn untouched(&self, seed: &State, setup: &dyn Fn(&Arena)) -> Outcome {
            self.reset_objects();
            setup(&self.arena);
            let state = self.machine.run(&self.arena, self.arena.at(IDENTITY), seed);
            self.outcome(state)
        }

        /// The native and hooked outcomes of one case.
        fn both(
            &self,
            hook: &Hook,
            controls: u32,
            seed: &State,
            setup: &dyn Fn(&Arena),
        ) -> (Outcome, Outcome) {
            (
                self.run(hook, false, controls, seed, setup),
                self.run(hook, true, controls, seed, setup),
            )
        }

        fn assert_deep_stack_untouched(&self) {
            let deep = self.arena.read(STACK, ENTRY - STACK_BELOW - STACK);
            assert!(
                deep.iter().all(|byte| *byte == CANARY),
                "a run wrote below its stack window"
            );
            let top = ENTRY + STACK_ABOVE;
            let high = self.arena.read(top, STACK + STACK_LEN - top);
            assert!(
                high.iter().all(|byte| *byte == CANARY),
                "a run wrote above its stack window"
            );
        }

        fn cave_range(&self, hook: &Hook) -> std::ops::Range<u64> {
            let range = hook.cave().range();
            range.start().get()..range.end()
        }

        fn class_bits(&self) -> u32 {
            METRO
                .classes
                .iter()
                .fold(0, |bits, class| bits | class.bit.word())
        }
    }

    /// Records entry RSP, the return address, RDX and a call count in `slot`; changes no register or flag.
    fn recorder(
        a: &mut iced_x86::code_asm::CodeAssembler,
        slot: u64,
    ) -> Result<(), iced_x86::IcedError> {
        a.push(rax)?;
        a.push(rcx)?;
        a.mov(rax, slot)?;
        a.lea(rcx, qword_ptr(rsp + 16))?;
        a.mov(qword_ptr(rax), rcx)?;
        a.mov(rcx, qword_ptr(rsp + 16))?;
        a.mov(qword_ptr(rax + 8), rcx)?;
        a.mov(qword_ptr(rax + 16), rdx)?;
        a.mov(rcx, qword_ptr(rax + 24))?;
        a.lea(rcx, qword_ptr(rcx + 1))?;
        a.mov(qword_ptr(rax + 24), rcx)?;
        a.pop(rcx)?;
        a.pop(rax)?;
        a.ret()
    }

    /// A seed: every pointer register a site dereferences points at the weapon, the rest carry patterns.
    fn seed(world: &World, variant: u64) -> State {
        let weapon = world.arena.at(WEAPON);
        let mix = |n: u64| {
            (n + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ variant.wrapping_mul(0xD6E8_FEB8_6659_FD93)
        };
        let mut state = State {
            gpr: std::array::from_fn(|i| mix(i as u64)),
            rflags: [0x0, 0x8D5, 0x41, 0x880, 0x15, 0x44][variant as usize % 6] & ARITHMETIC_FLAGS,
            mxcsr: 0x1F80 | (variant as u32 & 0x3F),
            xmm: std::array::from_fn(|i| {
                let lane = |k: u64| u128::from(mix(i as u64 * 4 + k) as u32);
                lane(0) | lane(1) << 32 | lane(2) << 64 | lane(3) << 96
            }),
        };
        // RDI keeps its pattern: S2 subtracts it from RBX, and a no-reload receiver gets the weapon from
        // `receiver_seed`.
        for register in [
            Register::RAX,
            Register::RCX,
            Register::RBX,
            Register::R8,
            Register::R14,
        ] {
            state.set(register, weapon);
        }
        state.set(Register::RSI, weapon + 0x10);
        state
    }

    /// The register holding the weapon at a no-reload or pressure site.
    fn receiver(hook: &Hook) -> Register {
        match hook.site().kind {
            HookKind::NoReload(ReloadKind::SharedR14) => Register::R14,
            HookKind::NoReload(ReloadKind::HellsingFireRdi) => Register::RDI,
            HookKind::NoReload(_)
            | HookKind::Pressure {
                kind: PressureKind::RecoveryRcx,
                ..
            } => Register::RCX,
            HookKind::Pressure { .. } => Register::RBX,
            HookKind::Gate { .. } => panic!("{} has no receiver", hook.site().id),
        }
    }

    /// A seed for `hook` with the weapon in its receiver only and the decoy in every other register a cave
    /// could mistake for it.
    fn receiver_seed(world: &World, variant: u64, hook: &Hook) -> State {
        let mut state = seed(world, variant);
        state.set(receiver(hook), world.arena.at(WEAPON));
        let decoy = world.arena.at(DECOY);
        for register in [Register::RCX, Register::RBX, Register::RDI, Register::R14] {
            if register != receiver(hook) {
                state.set(register, decoy);
            }
        }
        state
    }

    fn assert_case(hooked: &Outcome, want: &Outcome, what: &str) {
        if hooked != want {
            let mut report = String::new();
            for (i, (h, w)) in hooked.state.gpr.iter().zip(&want.state.gpr).enumerate() {
                if h != w {
                    report += &format!(" gpr{i}: {h:#x} want {w:#x};");
                }
            }
            for (i, (h, w)) in hooked.state.xmm.iter().zip(&want.state.xmm).enumerate() {
                if h != w {
                    report += &format!(" xmm{i}: {h:#034x} want {w:#034x};");
                }
            }
            if hooked.state.rflags != want.state.rflags {
                report += &format!(
                    " rflags {:#x} want {:#x};",
                    hooked.state.rflags, want.state.rflags
                );
            }
            if hooked.state.mxcsr != want.state.mxcsr {
                report += &format!(
                    " mxcsr {:#x} want {:#x};",
                    hooked.state.mxcsr, want.state.mxcsr
                );
            }
            for (name, h, w) in [
                ("weapon", &hooked.weapon, &want.weapon),
                ("config", &hooked.config, &want.config),
                ("observed", &hooked.observed, &want.observed),
                ("stack", &hooked.stack, &want.stack),
            ] {
                if let Some(at) = h.iter().zip(w).position(|(a, b)| a != b) {
                    report += &format!(" {name} differs at {at:#x};");
                }
            }
            panic!("{what}:{report}");
        }
    }

    const NO_SETUP: &dyn Fn(&Arena) = &|_| {};

    /// A named change to the stand-in game before a run.
    type Case<'a> = (&'a str, &'a dyn Fn(&Arena));

    #[test]
    fn the_machine_round_trips_every_register() {
        let world = World::new();
        for variant in 0..6 {
            let seed = seed(&world, variant);
            let out = world.untouched(&seed, NO_SETUP).state;
            let mut want = seed;
            want.gpr[RSP] = world.machine.entry(&world.arena) + 8;
            want.rflags |= 2;
            assert_eq!(out, want);
        }
    }

    /// For a gate site with bit `bit` set: what the hooked run must equal, given the native and untouched runs.
    fn gate_effect(hook: &Hook, seed: &State, native: &Outcome, untouched: &Outcome) -> Outcome {
        let HookKind::Gate { effect, .. } = hook.site().kind else {
            unreachable!()
        };
        match effect {
            GateEffect::ReturnTrue => {
                let mut want = untouched.clone();
                want.state
                    .set(Register::RAX, seed.get(Register::RAX) & !0xFF | 1);
                want
            }
            GateEffect::ReserveLoad => {
                // The load happens; the subtraction (and the flags it set) does not.
                let mut want = native.clone();
                want.state.set(Register::RBX, seed.get(Register::RBX));
                want.state.rflags = seed.rflags | 2;
                want
            }
            GateEffect::Throwable { .. } => {
                let mut want = native.clone();
                want.set_observation(Mock::Enabled, native.observation(Mock::Callee));
                want.set_observation(Mock::Callee, [0; 4]);
                want
            }
            GateEffect::ZeroXmm9 => {
                let mut want = native.clone();
                want.state.xmm[9] = 0;
                want.state.mxcsr = seed.mxcsr;
                want
            }
            GateEffect::SkipMaskCall | GateEffect::SkipBatteryCall { .. } => untouched.clone(),
            GateEffect::SkipMedkitAnd => {
                let mut want = native.clone();
                want.weapon = untouched.weapon.clone();
                want.state.rflags = seed.rflags | 2;
                want
            }
            GateEffect::GodBranch { .. } => {
                let mut want = untouched.clone();
                want.state.set(Register::RAX, 1);
                want
            }
        }
    }

    #[test]
    fn gates_run_the_game_code_when_off_and_their_effect_when_on() {
        let world = World::new();
        for id in 1..=10 {
            let hook = world.hook(id);
            let HookKind::Gate { bit, .. } = hook.site().kind else {
                panic!("S{id} is not a gate");
            };
            let others = 0x7F & !bit.word();
            for variant in 0..6 {
                let seed = seed(&world, variant);
                // God mode compares a byte of the object in RCX; cover both outcomes of the comparison.
                let god_byte = 2 + (variant as u8 % 2);
                let setup = move |a: &Arena| a.write(WEAPON + 0x1344, &[god_byte]);
                for controls in [0, others, others | 0x8000_0000, !bit.word()] {
                    let (native, hooked) = world.both(hook, controls, &seed, &setup);
                    assert_case(
                        &hooked,
                        &native,
                        &format!("S{id} off, controls {controls:#x}"),
                    );
                }
                let untouched = world.untouched(&seed, &setup);
                for controls in [bit.word(), u32::MAX] {
                    let (native, hooked) = world.both(hook, controls, &seed, &setup);
                    let want = gate_effect(hook, &seed, &native, &untouched);
                    assert_case(&hooked, &want, &format!("S{id} on, controls {controls:#x}"));
                }
            }
        }
    }

    #[test]
    fn calls_keep_the_original_return_address_and_stack() {
        let world = World::new();
        let seed = seed(&world, 1);
        for (id, mock) in [
            (3, Mock::Callee),
            (4, Mock::Callee),
            (6, Mock::Virtual),
            (9, Mock::Battery),
            (10, Mock::Battery),
        ] {
            let hook = world.hook(id);
            let (native, hooked) = world.both(hook, 0, &seed, NO_SETUP);
            let [callee_rsp, ret, _, count] = hooked.observation(mock);
            assert_eq!(count, 1, "S{id}");
            assert_eq!(
                ret,
                world.arena.at(world.cont(hook)),
                "S{id} return address"
            );
            assert_eq!(
                callee_rsp,
                native.observation(mock)[0],
                "S{id} callee stack"
            );
            // Called from the site, the callee starts 8 below the site's RSP.
            assert_eq!(
                callee_rsp,
                world.machine.entry(&world.arena) - 8,
                "S{id} alignment"
            );
        }
    }

    /// The selected no-reload path: the round count stays.
    fn selected(hook: &Hook, seed: &State, native: &Outcome) -> Outcome {
        let mut want = native.clone();
        let count = seed.get(Register::RAX) & 0xFFFF_FFFF;
        match hook.site().kind {
            HookKind::NoReload(ReloadKind::SharedR14) => {
                want.state.set(Register::RDI, seed.get(Register::RDI));
                want.state.rflags = seed.rflags | 2;
            }
            HookKind::NoReload(ReloadKind::DirectCallbackRcx) => {
                want.state.set(Register::RDX, count);
                let mut observed = want.observation(Mock::Setter);
                observed[2] = count;
                want.set_observation(Mock::Setter, observed);
            }
            HookKind::NoReload(_) => want.state.set(Register::RDX, count),
            _ => unreachable!(),
        }
        want
    }

    /// The direct callback's setter returns into the cave, not to the site's continuation; everything else
    /// matches. Returns `native` with the hooked return address, after checking it.
    fn returned_into_cave(
        world: &World,
        hook: &Hook,
        native: &Outcome,
        hooked: &Outcome,
    ) -> Outcome {
        let mut want = native.clone();
        if hook.cave().returns_into_cave() {
            let native_ret = native.observation(Mock::Setter)[1];
            let hooked_ret = hooked.observation(Mock::Setter)[1];
            assert_eq!(native_ret, world.arena.at(world.cont(hook)));
            assert!(
                world.cave_range(hook).contains(&hooked_ret),
                "the setter must return into the cave"
            );
            let mut observed = want.observation(Mock::Setter);
            observed[1] = hooked_ret;
            want.set_observation(Mock::Setter, observed);
        }
        want
    }

    #[test]
    fn no_reload_keeps_rounds_for_every_selected_class_and_alias() {
        let world = World::new();
        let all = world.class_bits();
        for id in 11..=14 {
            let hook = world.hook(id);
            let seed = receiver_seed(&world, u64::from(id), hook);
            for (class, data) in METRO.classes.iter().enumerate() {
                let bit = data.bit.word();
                let other = METRO.classes[(class + 1) % METRO.classes.len()].bit.word();
                for &value in &world.ids[class] {
                    let setup = move |a: &Arena| a.put_u32(CONFIG, value);
                    for (controls, chosen) in [
                        (0, false),
                        (0x7F | 0x8000_0000, false),
                        (other, false),
                        (bit, true),
                        (all, true),
                        (u32::MAX, true),
                    ] {
                        let (native, hooked) = world.both(hook, controls, &seed, &setup);
                        let base = returned_into_cave(&world, hook, &native, &hooked);
                        let want = if chosen {
                            selected(hook, &seed, &base)
                        } else {
                            base
                        };
                        assert_case(
                            &hooked,
                            &want,
                            &format!(
                                "S{id} class {} id {value:#x} controls {controls:#x}",
                                data.key
                            ),
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn no_reload_leaves_other_actors_and_unknown_classes_alone() {
        let world = World::new();
        let layout = METRO.player;
        let owner = layout.weapon_owner as usize;
        let config = layout.weapon_config as usize;
        let cases: [Case<'_>; 7] = [
            ("another actor's weapon", &|a: &Arena| {
                a.put_u64(HANDLE, a.at(STRANGER) + u64::from(layout.owner))
            }),
            ("no owner handle", &move |a: &Arena| {
                a.put_u64(WEAPON + owner, 0)
            }),
            ("a null owner", &|a: &Arena| a.put_u64(HANDLE, 0)),
            ("no manager", &|a: &Arena| {
                a.put_u64(layout.manager.get() as usize, 0)
            }),
            ("no player", &|a: &Arena| {
                a.put_u64(MANAGER + layout.player as usize, 0)
            }),
            ("no configuration", &move |a: &Arena| {
                a.put_u64(WEAPON + config, 0)
            }),
            ("an unknown class", &|a: &Arena| {
                a.put_u32(CONFIG, 0x0BAD_C1A5)
            }),
        ];
        for id in 11..=14 {
            let hook = world.hook(id);
            let seed = receiver_seed(&world, 3, hook);
            for (what, setup) in cases {
                let (native, hooked) = world.both(hook, u32::MAX, &seed, setup);
                let want = returned_into_cave(&world, hook, &native, &hooked);
                assert_case(&hooked, &want, &format!("S{id} with {what}"));
            }
        }
    }

    #[test]
    fn disabled_hooks_never_read_the_weapon() {
        let world = World::new();
        world.arena.guard(GUARD, PAGE);
        world.arena.guard(GUARD2, PAGE);
        let classes = world.class_bits();
        for id in 11..=17 {
            let hook = world.hook(id);
            let (exposed, off, on) = match hook.site().kind {
                HookKind::Pressure { bit, .. } => (EXPOSED2, !bit.word(), bit.word()),
                _ => (EXPOSED, !classes, classes),
            };
            let mut seed = receiver_seed(&world, 2, hook);
            seed.set(receiver(hook), world.arena.at(exposed));
            world.reset_objects();
            world.install(hook, true);
            let target = world.arena.at(world.image(hook.site().rva));
            for (controls, survives) in [(0, true), (off, true), (on, false)] {
                world.arena.put_u32(CONTROL_WORD, controls);
                let ending = forked(|| {
                    world.machine.enter(&world.arena, target, &seed);
                    0
                });
                let want = if survives {
                    Ending::Exited(0)
                } else {
                    Ending::Signalled(libc::SIGSEGV)
                };
                assert_eq!(ending, want, "S{id} controls {controls:#x}");
            }
        }
    }

    #[test]
    fn a_setter_blocked_across_restore_returns_through_the_retained_cave() {
        let world = World::new();
        let hook = world.hook(12);
        let machine2 = Machine::new(&world.arena, MACHINE2, BLOCK2, ENTRY2);
        let class = world.ids[5][0];
        let blocker = |a: &Arena| {
            a.put_u32(CONFIG, class);
            a.put_u64(WEAPON + 0x1360, a.at(BLOCKER));
            a.put_u64(DECOY + 0x1360, a.at(BLOCKER));
        };
        for controls in [0, METRO.classes[5].bit.word()] {
            let seed = receiver_seed(&world, 4, hook);
            // The native reference runs with the setter released from the start.
            let native = world.run(hook, false, controls, &seed, &blocker);
            world.reset_objects();
            blocker(&world.arena);
            world.arena.put_u32(RENDEZVOUS + 4, 0);
            world.arena.put_u32(CONTROL_WORD, controls);
            world.install(hook, true);
            let target = world.arena.at(world.image(hook.site().rva));
            let hooked_state = std::thread::scope(|scope| {
                let thread = scope.spawn(|| {
                    // The second machine has its own stack; translate its entry RSP afterwards.
                    machine2.run(&world.arena, target, &seed)
                });
                let deadline = Instant::now() + Duration::from_secs(30);
                while world.arena.load_u32(RENDEZVOUS) == 0 {
                    assert!(Instant::now() < deadline, "the setter was never entered");
                    std::thread::yield_now();
                }
                // Detach while the setter runs: zero the controls and restore the original bytes. The cave
                // stays mapped.
                world.arena.store_u32(CONTROL_WORD, 0);
                world.install(hook, false);
                world.arena.store_u32(RENDEZVOUS + 4, 1);
                thread.join().unwrap()
            });
            let observed = world.arena.read(OBSERVED, OBSERVED_LEN);
            let [_, ret, setter_rdx, count] = std::array::from_fn(|i| {
                u64::from_le_bytes(
                    observed[Mock::Setter as usize * 0x40 + 8 * i..][..8]
                        .try_into()
                        .unwrap(),
                )
            });
            assert_eq!(count, 1);
            assert!(
                world.cave_range(hook).contains(&ret),
                "the setter returned into the cave"
            );
            let mut want = native.state;
            want.gpr[RSP] = machine2.entry(&world.arena) + 8;
            if controls != 0 {
                want.set(Register::RDX, seed.get(Register::RAX) & 0xFFFF_FFFF);
                assert_eq!(setter_rdx, seed.get(Register::RAX) & 0xFFFF_FFFF);
            }
            assert_eq!(hooked_state, want, "controls {controls:#x}");
        }
    }

    /// The bits of the pressures and normal pressures each pressure case combines.
    const PRESSURES: [u32; 13] = [
        0x0000_0000,
        0x8000_0000,
        0x4000_0000,
        0x412C_CCCD,
        0x41A0_0000,
        0xC000_0000,
        0x0000_0001,
        0x8000_0001,
        0x7F80_0000,
        0xFF80_0000,
        0x7FC0_0000,
        0x7FA0_0000,
        0x7F7F_FFFF,
    ];
    const NORMALS: [u32; 9] = [
        0x412C_CCCD,
        0x0000_0001,
        0xBF80_0000,
        0x0000_0000,
        0x8000_0000,
        0x7FC0_0000,
        0x7F80_0000,
        0xFF80_0000,
        0x4000_0000,
    ];

    /// `bits` as the comparison sees it: a denormal is zero under DAZ.
    fn compared(bits: u32, daz: bool) -> f32 {
        let value = f32::from_bits(bits);
        if daz && value.is_subnormal() {
            0.0_f32.copysign(value)
        } else {
            value
        }
    }

    /// Whether the correction floors `pressure` to `normal`.
    fn floors(pressure: u32, normal: u32, daz: bool) -> bool {
        let valid = (normal as i32) > 0 && normal < 0x7F80_0000;
        let pressure = compared(pressure, daz);
        valid && !pressure.is_nan() && compared(normal, daz) > pressure
    }

    fn helsing(world: &World) -> u32 {
        let class = METRO
            .classes
            .iter()
            .position(|class| class.key == METRO.player.pressure_class)
            .unwrap();
        world.ids[class][0]
    }

    #[test]
    fn pressure_floors_like_the_game_in_every_rounding_and_denormal_mode() {
        let world = World::new();
        let layout = METRO.player;
        let id = helsing(&world);
        let mut cases = 0;
        for site in 15..=17 {
            let hook = world.hook(site);
            let HookKind::Pressure { bit, kind } = hook.site().kind else {
                panic!("S{site} is not a pressure site");
            };
            for (index, &pressure) in PRESSURES.iter().enumerate() {
                for &normal in &NORMALS {
                    for mode in 0..16_u32 {
                        let mut seed = receiver_seed(&world, u64::from(mode), hook);
                        let daz = mode & 1 != 0;
                        // Rounding mode, DAZ, FTZ and some sticky flags; every exception masked.
                        seed.mxcsr = 0x1F80
                            | ((mode >> 2) << 13)
                            | if daz { 0x40 } else { 0 }
                            | if mode & 2 != 0 { 0x8000 } else { 0 }
                            | (index as u32 & 0x3F);
                        // The consumption subtracts 4.0 in its low lane.
                        for register in [6, 7] {
                            seed.xmm[register] =
                                seed.xmm[register] & !0xFFFF_FFFF | u128::from(0x4080_0000_u32);
                        }
                        let setup = move |a: &Arena| {
                            a.put_u32(CONFIG, id);
                            a.put_u32(CONFIG + layout.config_normal_pressure as usize, normal);
                            a.put_u32(WEAPON + layout.weapon_pressure as usize, pressure);
                        };
                        for controls in [!bit.word(), bit.word()] {
                            let (native, hooked) = world.both(hook, controls, &seed, &setup);
                            let mut want = native.clone();
                            if controls & bit.word() != 0 {
                                match kind {
                                    PressureKind::RecoveryRcx => {
                                        if floors(pressure, normal, daz) {
                                            let at = layout.weapon_pressure as usize;
                                            want.weapon[at..at + 4]
                                                .copy_from_slice(&normal.to_le_bytes());
                                        }
                                    }
                                    PressureKind::Consumption1RbxXmm1
                                    | PressureKind::Consumption2RbxXmm0 => {
                                        let register = if kind == PressureKind::Consumption1RbxXmm1
                                        {
                                            1
                                        } else {
                                            0
                                        };
                                        let result = native.state.xmm[register] as u32;
                                        if floors(result, normal, daz) {
                                            want.state.xmm[register] = native.state.xmm[register]
                                                & !0xFFFF_FFFF
                                                | u128::from(normal);
                                        }
                                    }
                                }
                            }
                            assert_case(
                                &hooked,
                                &want,
                                &format!(
                                    "S{site} pressure {pressure:#x} normal {normal:#x} mxcsr {:#x} controls {controls:#x}",
                                    seed.mxcsr
                                ),
                            );
                            cases += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(cases, 3 * PRESSURES.len() * NORMALS.len() * 16 * 2);
    }

    #[test]
    fn pressure_leaves_other_weapons_alone() {
        let world = World::new();
        let layout = METRO.player;
        let id = helsing(&world);
        let owner = layout.weapon_owner as usize;
        let config = layout.weapon_config as usize;
        let other_class = world.ids[0][0];
        let cases: [Case<'_>; 7] = [
            ("another actor's weapon", &|a: &Arena| {
                a.put_u64(HANDLE, a.at(STRANGER) + u64::from(layout.owner))
            }),
            ("no owner handle", &move |a: &Arena| {
                a.put_u64(WEAPON + owner, 0)
            }),
            ("a null owner", &|a: &Arena| a.put_u64(HANDLE, 0)),
            ("no manager", &|a: &Arena| {
                a.put_u64(layout.manager.get() as usize, 0)
            }),
            ("no player", &|a: &Arena| {
                a.put_u64(MANAGER + layout.player as usize, 0)
            }),
            ("no configuration", &move |a: &Arena| {
                a.put_u64(WEAPON + config, 0)
            }),
            ("another class", &move |a: &Arena| {
                a.put_u32(CONFIG, other_class)
            }),
        ];
        for site in 15..=17 {
            let hook = world.hook(site);
            let seed = receiver_seed(&world, 5, hook);
            for (what, change) in cases {
                let setup = |a: &Arena| {
                    a.put_u32(CONFIG, id);
                    a.put_u32(CONFIG + layout.config_normal_pressure as usize, 0x412C_CCCD);
                    a.put_u32(WEAPON + layout.weapon_pressure as usize, 0xC000_0000);
                    change(a);
                };
                let (native, hooked) = world.both(hook, u32::MAX, &seed, &setup);
                assert_case(&hooked, &native, &format!("S{site} with {what}"));
            }
        }
    }

    /// Whether `subss` on a signalling NaN with invalid unmasked raises SIGFPE here.
    fn simd_exceptions_trap(world: &World) -> bool {
        let probe = MOCKS + 0x900;
        world.arena.code(probe, &[0xF3, 0x0F, 0x5C, 0xC1, 0xC3]); // subss xmm0, xmm1; ret
        let mut seed = seed(world, 0);
        seed.xmm[1] = u128::from(0x7FA0_0000_u32);
        seed.mxcsr = 0x1F80 & !0x80;
        let target = world.arena.at(probe);
        forked(|| {
            world.machine.enter(&world.arena, target, &seed);
            0
        }) == Ending::Signalled(libc::SIGFPE)
    }

    #[test]
    fn pressure_traps_exactly_when_the_game_does() {
        let world = World::new();
        let layout = METRO.player;
        let id = helsing(&world);
        let mut trapped = 0;
        for site in 15..=17 {
            let hook = world.hook(site);
            let target = world.arena.at(world.image(hook.site().rva));
            for pressure in [0x7FA0_0000_u32, 0x0000_0001, 0x8000_0001, 0x4000_0000] {
                for normal in [0x412C_CCCD_u32, 0x0000_0001] {
                    // Invalid unmasked, denormal unmasked, everything unmasked, and everything unmasked
                    // under DAZ and FTZ.
                    for mxcsr in [0x1F80 & !0x80, 0x1F80 & !0x100, 0x0000, 0x8040] {
                        let mut seed = receiver_seed(&world, 0, hook);
                        seed.mxcsr = mxcsr;
                        // The consumption subtracts 4.0, which is exact for every pressure here.
                        for register in [6, 7] {
                            seed.xmm[register] =
                                seed.xmm[register] & !0xFFFF_FFFF | u128::from(0x4080_0000_u32);
                        }
                        let endings: Vec<Ending> = [false, true]
                            .into_iter()
                            .map(|hooked| {
                                world.reset_objects();
                                world.arena.put_u32(CONFIG, id);
                                world.arena.put_u32(
                                    CONFIG + layout.config_normal_pressure as usize,
                                    normal,
                                );
                                world
                                    .arena
                                    .put_u32(WEAPON + layout.weapon_pressure as usize, pressure);
                                world.arena.put_u32(CONTROL_WORD, u32::MAX);
                                world.install(hook, hooked);
                                forked(|| {
                                    world.machine.enter(&world.arena, target, &seed);
                                    0
                                })
                            })
                            .collect();
                        let what = format!(
                            "S{site} pressure {pressure:#x} normal {normal:#x} mxcsr {mxcsr:#x}"
                        );
                        assert_eq!(endings[0], endings[1], "{what}");
                        match endings[0] {
                            Ending::Exited(0) => {}
                            Ending::Signalled(libc::SIGFPE) if site != 15 => trapped += 1,
                            other => panic!("{what}: unexpected {other:?}"),
                        }
                    }
                }
            }
        }
        // The consumption's own subtraction traps on a signalling NaN or a denormal with that exception
        // unmasked, so wherever the platform raises SIMD exceptions the comparison is not vacuous. (QEMU's user
        // mode does not raise them.)
        if simd_exceptions_trap(&world) {
            assert!(trapped > 0, "no case trapped");
        }
    }
}
